// The incidents table and its helpers (`models::incident`).
// Requires `docker compose up -d postgres`.
mod common;

use std::time::Duration;

use chrono::{DateTime, Utc};
use common::{seed_http_monitor, seed_project};
use nami_engine::models::incident::{open_for_monitor, resolve_for_monitor};
use sqlx::PgPool;
use uuid::Uuid;

async fn open(pool: &PgPool, project: Uuid, monitor: Uuid, title: &str) -> Option<Uuid> {
    let mut conn = pool.acquire().await.unwrap();
    open_for_monitor(
        &mut conn,
        project,
        monitor,
        title,
        Some("cause"),
        Utc::now(),
    )
    .await
    .unwrap()
}

async fn resolve(pool: &PgPool, monitor: Uuid, at: DateTime<Utc>) -> Option<Uuid> {
    let mut conn = pool.acquire().await.unwrap();
    resolve_for_monitor(&mut conn, monitor, at).await.unwrap()
}

/// (title, status, resolved) of every incident of the monitor, oldest first.
async fn rows_of(pool: &PgPool, monitor: Uuid) -> Vec<(String, String, bool)> {
    sqlx::query_as(
        "select title, status, resolved_at is not null from incidents
         where monitor_id = $1 order by started_at, id",
    )
    .bind(monitor)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test]
async fn opening_twice_keeps_the_first_incident_and_reports_none(pool: PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9/", 1).await;

    let first = open(&pool, project, monitor, "first").await;
    let second = open(&pool, project, monitor, "second").await;

    assert!(first.is_some());
    assert_eq!(second, None);
    assert_eq!(
        rows_of(&pool, monitor).await,
        vec![("first".to_string(), "open".to_string(), false)]
    );
}

#[sqlx::test]
async fn resolving_closes_the_open_incident_at_the_given_time(pool: PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9/", 1).await;
    let opened = open(&pool, project, monitor, "down").await;
    let at = DateTime::from_timestamp(1_792_000_000, 0).unwrap();

    let resolved = resolve(&pool, monitor, at).await;

    assert_eq!(resolved, opened);
    let stored: (String, DateTime<Utc>) =
        sqlx::query_as("select status, resolved_at from incidents where id = $1")
            .bind(opened.unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, ("resolved".to_string(), at));
    assert_eq!(
        resolve(&pool, monitor, at).await,
        None,
        "a resolved incident is not resolved again"
    );
}

#[sqlx::test]
async fn resolving_without_an_open_incident_does_nothing(pool: PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9/", 1).await;

    assert_eq!(resolve(&pool, monitor, Utc::now()).await, None);
    assert!(rows_of(&pool, monitor).await.is_empty());
}

#[sqlx::test]
async fn a_resolved_incident_does_not_block_the_next_one(pool: PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9/", 1).await;
    let first = open(&pool, project, monitor, "first outage").await;
    resolve(&pool, monitor, Utc::now()).await;

    let second = open(&pool, project, monitor, "second outage").await;

    assert!(second.is_some());
    assert_ne!(first, second);
    let rows = rows_of(&pool, monitor).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .filter(|(_, status, _)| status == "open")
            .count(),
        1
    );
}

#[sqlx::test]
async fn incidents_of_different_monitors_do_not_interfere(pool: PgPool) {
    let project = seed_project(&pool).await;
    let a = seed_http_monitor(&pool, project, "a", "http://127.0.0.1:9/", 1).await;
    let b = seed_http_monitor(&pool, project, "b", "http://127.0.0.1:9/", 1).await;

    assert!(open(&pool, project, a, "a down").await.is_some());
    assert!(open(&pool, project, b, "b down").await.is_some());
    assert!(resolve(&pool, a, Utc::now()).await.is_some());

    assert_eq!(rows_of(&pool, a).await[0].1, "resolved");
    assert_eq!(rows_of(&pool, b).await[0].1, "open");
}

/// The unique index only constrains monitor incidents: rows without a monitor
/// (heartbeat incidents, added by the heartbeats migration) never collide,
/// because NULLs are distinct.
#[sqlx::test]
async fn open_incidents_without_a_monitor_do_not_collide(pool: PgPool) {
    let project = seed_project(&pool).await;
    for title in ["one", "two"] {
        sqlx::query("insert into incidents (project_id, title) values ($1, $2)")
            .bind(project)
            .bind(title)
            .execute(&pool)
            .await
            .unwrap();
    }
    let open: i64 = sqlx::query_scalar("select count(*) from incidents where status = 'open'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(open, 2);
}

/// Two writers racing to open an incident for one monitor: the second waits
/// for the first and then finds the incident already there. Whichever way
/// the first transaction ends decides the second's result, so this is
/// deterministic: the unique index is what serializes them.
#[sqlx::test]
async fn a_racing_open_waits_for_the_first_writer_and_finds_its_incident(pool: PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9/", 1).await;

    let mut first = pool.begin().await.unwrap();
    let first_id = open_for_monitor(&mut first, project, monitor, "first", None, Utc::now())
        .await
        .unwrap();
    assert!(first_id.is_some());

    let racing = tokio::spawn({
        let pool = pool.clone();
        async move { open(&pool, project, monitor, "racing").await }
    });
    // The first writer has not committed: the racing insert cannot know yet
    // whether the incident exists, so it must still be waiting.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !racing.is_finished(),
        "the racing open must wait for the first writer"
    );

    first.commit().await.unwrap();

    assert_eq!(racing.await.unwrap(), None);
    assert_eq!(
        rows_of(&pool, monitor).await,
        vec![("first".to_string(), "open".to_string(), false)]
    );
}

#[sqlx::test]
async fn a_racing_open_goes_ahead_when_the_first_writer_rolls_back(pool: PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9/", 1).await;

    let mut first = pool.begin().await.unwrap();
    open_for_monitor(&mut first, project, monitor, "first", None, Utc::now())
        .await
        .unwrap();
    let racing = tokio::spawn({
        let pool = pool.clone();
        async move { open(&pool, project, monitor, "racing").await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !racing.is_finished(),
        "the racing open must wait for the first writer"
    );

    first.rollback().await.unwrap();

    assert!(racing.await.unwrap().is_some());
    assert_eq!(
        rows_of(&pool, monitor).await,
        vec![("racing".to_string(), "open".to_string(), false)]
    );
}

#[sqlx::test]
async fn status_is_limited_to_open_and_resolved(pool: PgPool) {
    let project = seed_project(&pool).await;
    let result =
        sqlx::query("insert into incidents (project_id, title, status) values ($1, 'x', 'bogus')")
            .bind(project)
            .execute(&pool)
            .await;
    let error = result.unwrap_err();
    // 23514 = check_violation
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23514"),
        "{error}"
    );
}

#[sqlx::test]
async fn deleting_a_monitor_or_a_project_deletes_its_incidents(pool: PgPool) {
    let project = seed_project(&pool).await;
    let doomed = seed_http_monitor(&pool, project, "doomed", "http://127.0.0.1:9/", 1).await;
    let kept = seed_http_monitor(&pool, project, "kept", "http://127.0.0.1:9/", 1).await;
    open(&pool, project, doomed, "doomed down").await;
    open(&pool, project, kept, "kept down").await;

    sqlx::query("delete from monitors where id = $1")
        .bind(doomed)
        .execute(&pool)
        .await
        .unwrap();
    assert!(rows_of(&pool, doomed).await.is_empty());
    assert_eq!(rows_of(&pool, kept).await.len(), 1);

    sqlx::query("delete from projects where id = $1")
        .bind(project)
        .execute(&pool)
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar("select count(*) from incidents")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
}
