// The incident lifecycle (incidents::lifecycle). Requires `docker compose up -d postgres`.
use crate::common;

use chrono::{DateTime, Utc};
use common::{seed_http_monitor, seed_project, seed_user, unique_email};
use nami_engine::incidents::lifecycle::{self, IncidentSource, OpenIncident, Reopen};
use nami_engine::incidents::model as incident;
use sqlx::PgPool;
use uuid::Uuid;

fn at(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(1_792_000_000 + seconds, 0).unwrap()
}

async fn open(pool: &PgPool, project: Uuid, source: IncidentSource) -> Option<Uuid> {
    let mut conn = pool.acquire().await.unwrap();
    lifecycle::open(
        &mut conn,
        &OpenIncident {
            project_id: project,
            source,
            title: "api is down",
            cause: Some("connection refused"),
            severity_id: None,
            metadata: None,
            started_at: at(0),
        },
    )
    .await
    .unwrap()
}

async fn events(pool: &PgPool, incident: Uuid) -> Vec<(String, Option<Uuid>)> {
    sqlx::query_as("select kind, actor_id from incident_events where incident_id = $1 order by id")
        .bind(incident)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn status(pool: &PgPool, incident: Uuid) -> (String, Option<Uuid>, Option<Uuid>) {
    sqlx::query_as("select status, acknowledged_by, resolved_by from incidents where id = $1")
        .bind(incident)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn opening_records_source_default_severity_and_an_opened_event(pool: PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9", 1).await;
    let id = open(&pool, project, IncidentSource::Monitor(monitor))
        .await
        .unwrap();

    let (source, severity, monitor_id): (String, String, Option<Uuid>) = sqlx::query_as(
        "select i.source, s.name, i.monitor_id from incidents i
         join incident_severities s on s.id = i.severity_id where i.id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (source.as_str(), severity.as_str(), monitor_id),
        ("monitor", "high", Some(monitor))
    );
    assert_eq!(events(&pool, id).await, [("opened".to_string(), None)]);
}

#[sqlx::test]
async fn a_severity_of_another_project_falls_back_to_the_default(pool: PgPool) {
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let foreign: Uuid = sqlx::query_scalar(
        "select id from incident_severities where project_id = $1 and name = 'critical'",
    )
    .bind(other)
    .fetch_one(&pool)
    .await
    .unwrap();
    let user = seed_user(&pool, &unique_email("m"), "member").await;
    let mut conn = pool.acquire().await.unwrap();
    let id = lifecycle::open(
        &mut conn,
        &OpenIncident {
            project_id: project,
            source: IncidentSource::Manual { created_by: user },
            title: "manual",
            cause: None,
            severity_id: Some(foreign),
            metadata: None,
            started_at: at(0),
        },
    )
    .await
    .unwrap()
    .unwrap();
    let (name, created_by): (String, Option<Uuid>) = sqlx::query_as(
        "select s.name, i.created_by from incidents i join incident_severities s on s.id = i.severity_id
         where i.id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((name.as_str(), created_by), ("high", Some(user)));
}

#[sqlx::test]
async fn a_source_has_at_most_one_unresolved_incident_even_while_acknowledged(pool: PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9", 1).await;
    let user = seed_user(&pool, &unique_email("m"), "member").await;
    let first = open(&pool, project, IncidentSource::Monitor(monitor))
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    assert!(
        lifecycle::acknowledge(&mut conn, first, user, at(10))
            .await
            .unwrap()
    );
    assert_eq!(open(&pool, project, IncidentSource::Monitor(monitor)).await, None);
    assert_eq!(
        lifecycle::resolve_source(&mut conn, &IncidentSource::Monitor(monitor), at(20))
            .await
            .unwrap(),
        Some(first)
    );
    assert!(
        open(&pool, project, IncidentSource::Monitor(monitor))
            .await
            .is_some()
    );
}

#[sqlx::test]
async fn acknowledge_only_from_open_and_resolve_from_either_state(pool: PgPool) {
    let project = seed_project(&pool).await;
    let user = seed_user(&pool, &unique_email("m"), "member").await;
    let id = open(&pool, project, IncidentSource::Manual { created_by: user })
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();

    assert!(lifecycle::acknowledge(&mut conn, id, user, at(5)).await.unwrap());
    assert!(
        !lifecycle::acknowledge(&mut conn, id, user, at(6)).await.unwrap(),
        "already acknowledged"
    );
    assert_eq!(
        status(&pool, id).await,
        ("acknowledged".into(), Some(user), None)
    );

    assert!(
        lifecycle::resolve(&mut conn, id, Some(user), at(7))
            .await
            .unwrap()
    );
    assert!(
        !lifecycle::resolve(&mut conn, id, Some(user), at(8))
            .await
            .unwrap(),
        "already resolved"
    );
    assert!(
        !lifecycle::acknowledge(&mut conn, id, user, at(9)).await.unwrap(),
        "resolved is not open"
    );
    assert_eq!(
        status(&pool, id).await,
        ("resolved".into(), Some(user), Some(user))
    );
    // A manual incident's `opened` event names its creator.
    assert_eq!(
        events(&pool, id).await,
        [
            ("opened".into(), Some(user)),
            ("acknowledged".into(), Some(user)),
            ("resolved".into(), Some(user))
        ]
    );
}

#[sqlx::test]
async fn automatic_resolution_has_no_resolver(pool: PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9", 1).await;
    let id = open(&pool, project, IncidentSource::Monitor(monitor))
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    lifecycle::resolve_source(&mut conn, &IncidentSource::Monitor(monitor), at(30))
        .await
        .unwrap();
    assert_eq!(status(&pool, id).await, ("resolved".into(), None, None));
    assert_eq!(
        events(&pool, id).await.last().unwrap(),
        &("resolved".to_string(), None)
    );
}

#[sqlx::test]
async fn reopen_returns_to_open_clears_the_acknowledgement_and_respects_the_source(pool: PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9", 1).await;
    let user = seed_user(&pool, &unique_email("m"), "member").await;
    let first = open(&pool, project, IncidentSource::Monitor(monitor))
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();

    assert!(matches!(
        lifecycle::reopen(&mut conn, first, user, at(1)).await.unwrap(),
        Reopen::NotResolved
    ));
    lifecycle::acknowledge(&mut conn, first, user, at(2))
        .await
        .unwrap();
    lifecycle::resolve(&mut conn, first, Some(user), at(3))
        .await
        .unwrap();
    assert!(matches!(
        lifecycle::reopen(&mut conn, first, user, at(4)).await.unwrap(),
        Reopen::Reopened
    ));
    let (status, ack_by, resolved_by) = status(&pool, first).await;
    assert_eq!((status.as_str(), ack_by, resolved_by), ("open", None, None));
    let resolved_at: Option<DateTime<Utc>> =
        sqlx::query_scalar("select resolved_at from incidents where id = $1")
            .bind(first)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(resolved_at, None);

    // A newer incident of the same monitor blocks reopening the old one.
    lifecycle::resolve(&mut conn, first, None, at(5))
        .await
        .unwrap();
    let second = open(&pool, project, IncidentSource::Monitor(monitor))
        .await
        .unwrap();
    assert_ne!(first, second);
    assert!(matches!(
        lifecycle::reopen(&mut conn, first, user, at(6)).await.unwrap(),
        Reopen::SourceHasUnresolvedIncident
    ));
    assert!(matches!(
        lifecycle::reopen(&mut conn, Uuid::new_v4(), user, at(7))
            .await
            .unwrap(),
        Reopen::NotFound
    ));
}

#[sqlx::test]
async fn comments_are_events_with_their_author(pool: PgPool) {
    let project = seed_project(&pool).await;
    let user = seed_user(&pool, &unique_email("m"), "member").await;
    let id = open(&pool, project, IncidentSource::Manual { created_by: user })
        .await
        .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    lifecycle::comment(&mut conn, id, user, "looking into it")
        .await
        .unwrap();
    let (kind, actor, message): (String, Option<Uuid>, Option<String>) = sqlx::query_as(
        "select kind, actor_id, message from incident_events where incident_id = $1 order by id desc limit 1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (kind.as_str(), actor, message.as_deref()),
        ("comment", Some(user), Some("looking into it"))
    );
}

#[sqlx::test]
async fn racing_opens_for_one_source_create_one_incident(pool: PgPool) {
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9", 1).await;
    let (a, b) = tokio::join!(
        open(&pool, project, IncidentSource::Monitor(monitor)),
        open(&pool, project, IncidentSource::Monitor(monitor))
    );
    assert_eq!(a.is_some() as u8 + b.is_some() as u8, 1);
    let count: i64 = sqlx::query_scalar("select count(*) from incidents where monitor_id = $1")
        .bind(monitor)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[sqlx::test]
async fn a_heartbeat_incident_resolved_moments_ago_is_reopened_with_an_event(pool: PgPool) {
    let project = seed_project(&pool).await;
    let heartbeat: Uuid = sqlx::query_scalar(
        "insert into heartbeats (project_id, name, token, period_seconds, grace_seconds)
         values ($1, 'backup', md5(gen_random_uuid()::text) || md5(gen_random_uuid()::text), 60, 0)
         returning id",
    )
    .bind(project)
    .fetch_one(&pool)
    .await
    .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    let first = incident::open_for_heartbeat(
        &mut conn,
        project,
        heartbeat,
        "backup missed its heartbeat",
        Some("no ping"),
        at(0),
    )
    .await
    .unwrap()
    .unwrap();
    incident::attach_details(&mut conn, first, "exit 1")
        .await
        .unwrap();
    incident::resolve_for_heartbeat(&mut conn, heartbeat, at(10))
        .await
        .unwrap();

    let again = incident::open_for_heartbeat(
        &mut conn,
        project,
        heartbeat,
        "backup reported a failure",
        Some("exit code 2"),
        at(70),
    )
    .await
    .unwrap();

    assert_eq!(again, Some(first));
    let row: (String, String, Option<String>, Option<String>) =
        sqlx::query_as("select status, title, cause, details from incidents where id = $1")
            .bind(first)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        row,
        (
            "open".into(),
            "backup reported a failure".into(),
            Some("exit code 2".into()),
            None
        )
    );
    assert_eq!(
        events(&pool, first).await.last().unwrap(),
        &("reopened".to_string(), None)
    );
}
