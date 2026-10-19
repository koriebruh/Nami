// Maintenance windows: CRUD, RBAC, project isolation, and how they freeze the
// monitor and heartbeat state machines. Requires
// `docker compose up -d postgres mailpit`.
mod common;

use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use common::{
    add_member, call, outcome, public_app, seed_http_monitor, seed_project, seed_user,
    settled_mail, test_app, unique_email, user_with_role,
};
use nami_engine::{
    core::state::AppState,
    uptime::monitor::model::Monitor,
    uptime::{
        alerts::TransitionKind,
        checker::{CheckOutcome, CheckStatus},
        worker::{apply_outcome, sweep_heartbeats},
    },
};
use serde_json::{Value, json};
use uuid::Uuid;

/// A one-off window that started 5 minutes ago and ends in an hour.
fn window_now(title: &str, monitor_id: Option<Uuid>) -> Value {
    json!({
        "title": title,
        "monitor_id": monitor_id,
        "kind": "once",
        "starts_at": (Utc::now() - Duration::minutes(5)).to_rfc3339(),
        "ends_at": (Utc::now() + Duration::hours(1)).to_rfc3339()
    })
}

/// Inserts a window covering now directly (bypassing the API).
async fn seed_window_now(pool: &sqlx::PgPool, project: Uuid, monitor_id: Option<Uuid>) -> Uuid {
    sqlx::query_scalar(
        "insert into maintenance_windows (project_id, monitor_id, title, kind, starts_at, ends_at)
         values ($1, $2, 'maintenance', 'once', now() - interval '5 minutes', now() + interval '1 hour')
         returning id",
    )
    .bind(project)
    .bind(monitor_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Ends a window: it ran from an hour ago to a second ago.
async fn end_window(pool: &sqlx::PgPool, window: Uuid) {
    sqlx::query(
        "update maintenance_windows
         set starts_at = now() - interval '1 hour', ends_at = now() - interval '1 second'
         where id = $1",
    )
    .bind(window)
    .execute(pool)
    .await
    .unwrap();
}

fn outcome_at(status: CheckStatus, checked_at: DateTime<Utc>) -> CheckOutcome {
    CheckOutcome {
        checked_at,
        ..outcome(status)
    }
}

async fn monitor(pool: &sqlx::PgPool, id: Uuid) -> Monitor {
    sqlx::query_as("select * from monitors where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// (status, consecutive_failures, consecutive_successes)
async fn counters(pool: &sqlx::PgPool, id: Uuid) -> (String, i32, i32) {
    sqlx::query_as(
        "select current_status, consecutive_failures, consecutive_successes from monitors where id = $1",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn open_incidents(pool: &sqlx::PgPool, monitor_id: Uuid) -> i64 {
    sqlx::query_scalar("select count(*) from incidents where monitor_id = $1 and status = 'open'")
        .bind(monitor_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn window_count(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar("select count(*) from maintenance_windows")
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn members_create_and_edit_one_off_and_weekly_windows_admins_delete(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let api_monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let collection = format!("/projects/{project}/maintenance-windows");

    let (status, once) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(window_now("  DB upgrade ", None)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{once}");
    assert_eq!(once["title"], "DB upgrade", "trimmed");
    assert_eq!(once["kind"], "once");
    assert_eq!(once["project_id"], project.to_string());
    assert_eq!(once["monitor_id"], Value::Null);
    assert_eq!(once["active"], true);
    assert_eq!(once["days_of_week"], Value::Null);

    let weekly = json!({
        "title": "Weekly deploy", "monitor_id": api_monitor, "kind": "weekly",
        "days_of_week": [4, 2, 2], "start_time": "22:00", "end_time": "02:00", "timezone": "Asia/Jakarta",
        "starts_at": "2026-01-01T00:00:00Z"
    });
    let (status, weekly) = call(&app, "POST", &collection, Some(&member), Some(weekly)).await;
    assert_eq!(status, StatusCode::CREATED, "{weekly}");
    assert_eq!(
        weekly["days_of_week"],
        json!([2, 4]),
        "sorted, duplicates dropped"
    );
    assert_eq!(weekly["start_time"], "22:00:00");
    assert_eq!(weekly["end_time"], "02:00:00");
    assert_eq!(weekly["timezone"], "Asia/Jakarta");
    assert_eq!(
        weekly["starts_at"],
        Value::Null,
        "one-off fields are dropped from weekly windows"
    );
    assert_eq!(weekly["monitor_id"], api_monitor.to_string());

    let (status, list) = call(&app, "GET", &collection, Some(&member), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 2);

    let item = format!("{collection}/{}", weekly["id"].as_str().unwrap());
    let (status, patched) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({
            "title": "Deploys", "monitor_id": null, "days_of_week": [1, 2, 3, 4, 5, 6, 7],
            "start_time": "00:00", "end_time": "00:00", "timezone": "UTC"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["title"], "Deploys");
    assert_eq!(
        patched["monitor_id"],
        Value::Null,
        "null makes the window project-wide"
    );
    assert_eq!(patched["active"], true, "every day, all day");

    // A patch without monitor_id keeps it; a patch of one field keeps the rest.
    let (status, renamed) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "title": "Nightly" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    assert_eq!(renamed["days_of_week"], json!([1, 2, 3, 4, 5, 6, 7]));
    assert_eq!(renamed["timezone"], "UTC");

    let (status, switched) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(window_now("Now", None)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{switched}");
    assert_eq!(switched["kind"], "once");
    assert_eq!(
        switched["days_of_week"],
        Value::Null,
        "weekly fields are dropped when switching kind"
    );
    assert_eq!(switched["timezone"], Value::Null);

    assert_eq!(
        call(&app, "DELETE", &item, Some(&member), None).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, body) = call(&app, "DELETE", &item, Some(&admin), None).await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "deleted": true })));
    assert_eq!(
        call(&app, "GET", &item, Some(&admin), None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "DELETE", &item, Some(&admin), None).await.0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn a_past_or_future_window_is_listed_as_inactive(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/maintenance-windows");
    let later = json!({
        "title": "Later", "kind": "once",
        "starts_at": (Utc::now() + Duration::days(1)).to_rfc3339(),
        "ends_at": (Utc::now() + Duration::days(2)).to_rfc3339()
    });
    let (status, created) = call(&app, "POST", &collection, Some(&member), Some(later)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["active"], false);
}

#[sqlx::test]
async fn viewers_read_and_outsiders_get_403(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let window = seed_window_now(&pool, project, None).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let collection = format!("/projects/{project}/maintenance-windows");
    let item = format!("{collection}/{window}");

    let (status, list) = call(&app, "GET", &collection, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0]["active"], true);
    assert_eq!(
        call(&app, "GET", &item, Some(&viewer), None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &collection,
            Some(&viewer),
            Some(window_now("x", None))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "PATCH",
            &item,
            Some(&viewer),
            Some(json!({ "title": "x" }))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "DELETE", &item, Some(&viewer), None).await.0,
        StatusCode::FORBIDDEN
    );
    for (method, uri, body) in [
        ("GET", &collection, None),
        ("POST", &collection, Some(window_now("x", None))),
        ("GET", &item, None),
        ("PATCH", &item, Some(json!({ "title": "x" }))),
        ("DELETE", &item, None),
    ] {
        assert_eq!(
            call(&app, method, uri, Some(&outsider), body).await.0,
            StatusCode::FORBIDDEN,
            "{method} {uri}"
        );
    }
    // Without a session: 401, and nothing changed.
    assert_eq!(
        call(&app, "GET", &collection, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(window_count(&pool).await, 1);
    let title: String = sqlx::query_scalar("select title from maintenance_windows")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(title, "maintenance");
}

#[sqlx::test]
async fn a_global_admin_manages_windows_without_membership(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let email = unique_email("root");
    seed_user(&pool, &email, "admin").await;
    let admin = common::login(&app, &email).await;
    let collection = format!("/projects/{project}/maintenance-windows");
    let (status, created) = call(
        &app,
        "POST",
        &collection,
        Some(&admin),
        Some(window_now("x", None)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    // A project that does not exist is a 404, not a 500.
    let missing = format!("/projects/{}/maintenance-windows", Uuid::new_v4());
    assert_eq!(
        call(
            &app,
            "POST",
            &missing,
            Some(&admin),
            Some(window_now("x", None))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}

/// Security: window ids and monitor ids of another project are 404s and
/// nothing is written or changed.
#[sqlx::test]
async fn windows_and_monitors_of_another_project_are_404(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let foreign_window = seed_window_now(&pool, other, None).await;
    let foreign_monitor = seed_http_monitor(&pool, other, "Other", "https://example.com", 2).await;
    let own_window = seed_window_now(&pool, project, None).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let collection = format!("/projects/{project}/maintenance-windows");
    let foreign_item = format!("{collection}/{foreign_window}");

    assert_eq!(
        call(&app, "GET", &foreign_item, Some(&admin), None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "PATCH",
            &foreign_item,
            Some(&admin),
            Some(json!({ "title": "hijacked" }))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "DELETE", &foreign_item, Some(&admin), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (status, _) = call(
        &app,
        "POST",
        &collection,
        Some(&admin),
        Some(window_now("x", Some(foreign_monitor))),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a monitor of another project cannot be targeted"
    );
    let (status, _) = call(
        &app,
        "POST",
        &collection,
        Some(&admin),
        Some(window_now("x", Some(Uuid::new_v4()))),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "nor one that does not exist");
    let own_item = format!("{collection}/{own_window}");
    let (status, _) = call(
        &app,
        "PATCH",
        &own_item,
        Some(&admin),
        Some(json!({ "monitor_id": foreign_monitor })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // The other project's list does not leak into this one.
    let (_, list) = call(&app, "GET", &collection, Some(&admin), None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], own_window.to_string());

    let windows: Vec<(Uuid, Option<Uuid>, String)> = sqlx::query_as(
        "select project_id, monitor_id, title from maintenance_windows order by project_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(windows.len(), 2, "nothing was created");
    assert!(
        windows
            .iter()
            .all(|(_, monitor_id, title)| monitor_id.is_none() && title == "maintenance")
    );
    // The database refuses a cross-project reference even without the API.
    let direct = sqlx::query(
        "insert into maintenance_windows (project_id, monitor_id, title, kind, starts_at, ends_at)
         values ($1, $2, 'x', 'once', now(), now() + interval '1 hour')",
    )
    .bind(project)
    .bind(foreign_monitor)
    .execute(&pool)
    .await;
    assert!(direct.is_err(), "the composite foreign key rejects it");
}

#[sqlx::test]
async fn deleting_a_monitor_deletes_its_windows_only(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let id = seed_http_monitor(&pool, project, "API", "https://example.com", 1).await;
    seed_window_now(&pool, project, Some(id)).await;
    let project_wide = seed_window_now(&pool, project, None).await;
    sqlx::query("delete from monitors where id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let left: Vec<Uuid> = sqlx::query_scalar("select id from maintenance_windows")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(left, [project_wide]);
}

#[sqlx::test]
async fn invalid_windows_are_rejected_and_nothing_is_written(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let window = seed_window_now(&pool, project, None).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let weekly = |extra: Value| {
        let mut body = json!({
            "title": "w", "kind": "weekly", "days_of_week": [1], "start_time": "01:00",
            "end_time": "02:00", "timezone": "UTC"
        });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };
    let once = |title: &str, starts_at: DateTime<Utc>, ends_at: DateTime<Utc>| json!({ "title": title, "kind": "once", "starts_at": starts_at, "ends_at": ends_at });
    let now = Utc::now();
    let hour = now + Duration::hours(1);
    let cases = [
        (once(" ", now, hour), "title is required"),
        (once("x\r\ny", now, hour), "control"),
        (once("a\u{202E}b", now, hour), "invisible or text-direction"),
        (once(&"x".repeat(201), now, hour), "at most 200 characters"),
        (
            json!({ "title": "x", "kind": "daily" }),
            "kind must be once or weekly",
        ),
        (
            json!({ "title": "x", "kind": "once", "starts_at": now }),
            "starts_at and ends_at are required",
        ),
        (once("x", now, now), "ends_at must be after starts_at"),
        (once("x", hour, now), "ends_at must be after starts_at"),
        (
            once("x", now, now + Duration::days(367)),
            "at most 366 days",
        ),
        (
            json!({ "title": "x", "kind": "once", "starts_at": "1969-12-31T00:00:00Z", "ends_at": "1970-01-02T00:00:00Z" }),
            "between the years",
        ),
        (weekly(json!({ "days_of_week": [] })), "days_of_week"),
        (weekly(json!({ "days_of_week": null })), "days_of_week"),
        (weekly(json!({ "days_of_week": [0] })), "days_of_week"),
        (weekly(json!({ "days_of_week": [8] })), "days_of_week"),
        (weekly(json!({ "days_of_week": [-1] })), "days_of_week"),
        (
            weekly(json!({ "start_time": "25:00" })),
            "start_time must be a time of day",
        ),
        (
            weekly(json!({ "start_time": "23:59:60" })),
            "start_time must be a time of day",
        ),
        (
            weekly(json!({ "start_time": "01:00:00.5" })),
            "start_time must be a time of day",
        ),
        (
            weekly(json!({ "end_time": "noon" })),
            "end_time must be a time of day",
        ),
        (
            weekly(json!({ "end_time": null })),
            "end_time must be a time of day",
        ),
        (
            weekly(json!({ "timezone": "Mars/Olympus_Mons" })),
            "timezone must be an IANA time zone",
        ),
        (
            weekly(json!({ "timezone": "utc+7" })),
            "timezone must be an IANA time zone",
        ),
        (
            weekly(json!({ "timezone": null })),
            "timezone must be an IANA time zone",
        ),
    ];
    let collection = format!("/projects/{project}/maintenance-windows");
    let item = format!("{collection}/{window}");
    for (body, fragment) in cases {
        let (status, json) =
            call(&app, "POST", &collection, Some(&member), Some(body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} returned {json}");
        let message = json["error"].as_str().unwrap_or_default();
        assert!(
            message.contains(fragment),
            "{body}: {message:?} should mention {fragment:?}"
        );
        // The same body as a patch of an existing window is refused too
        // (bar the missing ends_at: a patch keeps the window's own).
        if fragment.starts_with("starts_at and ends_at") {
            continue;
        }
        let (status, json) = call(&app, "PATCH", &item, Some(&member), Some(body.clone())).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "PATCH {body} returned {json}"
        );
    }
    assert_eq!(window_count(&pool).await, 1);
    let (title, kind): (String, String) =
        sqlx::query_as("select title, kind from maintenance_windows")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((title.as_str(), kind.as_str()), ("maintenance", "once"));
}

#[sqlx::test]
async fn a_covered_monitor_records_checks_but_its_state_never_moves(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let id = seed_http_monitor(&pool, project, "API", "http://127.0.0.1:9/", 1).await;
    seed_window_now(&pool, project, None).await;
    let m = monitor(&pool, id).await;

    for _ in 0..3 {
        assert_eq!(
            apply_outcome(&pool, &m, &outcome(CheckStatus::Down))
                .await
                .unwrap(),
            None
        );
    }
    let after = monitor(&pool, id).await;
    assert_eq!(
        counters(&pool, id).await,
        ("pending".to_string(), 0, 0),
        "counters frozen"
    );
    assert!(
        after.last_checked_at.is_some(),
        "the checks are still recorded"
    );
    assert_eq!(
        open_incidents(&pool, id).await,
        0,
        "no incident while in maintenance"
    );

    // A success does not move it out of pending either.
    assert_eq!(
        apply_outcome(&pool, &m, &outcome(CheckStatus::Up))
            .await
            .unwrap(),
        None
    );
    assert_eq!(counters(&pool, id).await, ("pending".to_string(), 0, 0));
}

#[sqlx::test]
async fn the_stale_outcome_guard_still_works_inside_a_window(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let id = seed_http_monitor(&pool, project, "API", "http://127.0.0.1:9/", 1).await;
    let window = seed_window_now(&pool, project, Some(id)).await;
    let m = monitor(&pool, id).await;
    let now = Utc::now();
    apply_outcome(&pool, &m, &outcome_at(CheckStatus::Down, now))
        .await
        .unwrap();
    assert_eq!(
        monitor(&pool, id)
            .await
            .last_checked_at
            .map(|t| t.timestamp_micros()),
        Some(now.timestamp_micros()),
        "the covered check moved last_checked_at"
    );
    end_window(&pool, window).await;
    // An outcome of a check that started before the covered one is stale.
    let stale = apply_outcome(
        &pool,
        &m,
        &outcome_at(CheckStatus::Down, now - Duration::seconds(1)),
    )
    .await
    .unwrap();
    assert_eq!(stale, None);
    assert_eq!(counters(&pool, id).await, ("pending".to_string(), 0, 0));
}

#[sqlx::test]
async fn a_weekly_window_freezes_the_monitor_too(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let id = seed_http_monitor(&pool, project, "API", "http://127.0.0.1:9/", 1).await;
    // Every day, all day, in a zone far from UTC.
    sqlx::query(
        "insert into maintenance_windows
             (project_id, monitor_id, title, kind, days_of_week, start_time, end_time, timezone)
         values ($1, $2, 'always', 'weekly', '{1,2,3,4,5,6,7}', '00:00', '00:00', 'Pacific/Kiritimati')",
    )
    .bind(project)
    .bind(id)
    .execute(&pool)
    .await
    .unwrap();
    let m = monitor(&pool, id).await;
    assert_eq!(
        apply_outcome(&pool, &m, &outcome(CheckStatus::Down))
            .await
            .unwrap(),
        None
    );
    assert_eq!(counters(&pool, id).await, ("pending".to_string(), 0, 0));
}

#[sqlx::test]
async fn a_window_scoped_to_one_monitor_leaves_the_others_alone(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let covered = seed_http_monitor(&pool, project, "Covered", "http://127.0.0.1:9/", 1).await;
    let other = seed_http_monitor(&pool, project, "Other", "http://127.0.0.1:9/", 1).await;
    let elsewhere_project = seed_project(&pool).await;
    let elsewhere = seed_http_monitor(
        &pool,
        elsewhere_project,
        "Elsewhere",
        "http://127.0.0.1:9/",
        1,
    )
    .await;
    seed_window_now(&pool, project, Some(covered)).await;
    // A project-wide window of another project covers nothing here.
    let third = seed_project(&pool).await;
    seed_window_now(&pool, third, None).await;

    let covered_result = apply_outcome(
        &pool,
        &monitor(&pool, covered).await,
        &outcome(CheckStatus::Down),
    )
    .await;
    let other_result = apply_outcome(
        &pool,
        &monitor(&pool, other).await,
        &outcome(CheckStatus::Down),
    )
    .await;
    let elsewhere_result = apply_outcome(
        &pool,
        &monitor(&pool, elsewhere).await,
        &outcome(CheckStatus::Down),
    )
    .await;

    assert_eq!(covered_result.unwrap(), None);
    assert_eq!(
        other_result.unwrap().map(|t| t.kind),
        Some(TransitionKind::Down)
    );
    assert_eq!(
        elsewhere_result.unwrap().map(|t| t.kind),
        Some(TransitionKind::Down)
    );
}

#[sqlx::test]
async fn an_incident_open_before_the_window_stays_open_and_resolves_after_it(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let id = seed_http_monitor(&pool, project, "API", "http://127.0.0.1:9/", 1).await;
    let m = monitor(&pool, id).await;
    assert_eq!(
        apply_outcome(&pool, &m, &outcome(CheckStatus::Down))
            .await
            .unwrap()
            .map(|t| t.kind),
        Some(TransitionKind::Down)
    );
    let window = seed_window_now(&pool, project, None).await;

    for _ in 0..2 {
        assert_eq!(
            apply_outcome(&pool, &m, &outcome(CheckStatus::Up))
                .await
                .unwrap(),
            None,
            "no recovery inside"
        );
    }
    assert_eq!(
        counters(&pool, id).await,
        ("down".to_string(), 1, 0),
        "successes inside do not count toward recovery"
    );
    assert_eq!(open_incidents(&pool, id).await, 1);

    end_window(&pool, window).await;
    assert_eq!(
        apply_outcome(&pool, &m, &outcome(CheckStatus::Up))
            .await
            .unwrap()
            .map(|t| t.kind),
        Some(TransitionKind::Recovered)
    );
    assert_eq!(open_incidents(&pool, id).await, 0);
}

#[sqlx::test]
async fn failures_inside_the_window_do_not_count_toward_the_threshold(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let id = seed_http_monitor(&pool, project, "API", "http://127.0.0.1:9/", 2).await;
    let m = monitor(&pool, id).await;
    apply_outcome(&pool, &m, &outcome(CheckStatus::Down))
        .await
        .unwrap();
    let window = seed_window_now(&pool, project, Some(id)).await;
    apply_outcome(&pool, &m, &outcome(CheckStatus::Down))
        .await
        .unwrap();
    assert_eq!(
        monitor(&pool, id).await.consecutive_failures,
        1,
        "the in-window failure did not count"
    );

    sqlx::query("delete from maintenance_windows where id = $1")
        .bind(window)
        .execute(&pool)
        .await
        .unwrap();
    let transition = apply_outcome(&pool, &m, &outcome(CheckStatus::Down))
        .await
        .unwrap();
    assert_eq!(
        transition.map(|t| t.kind),
        Some(TransitionKind::Down),
        "the second counted failure"
    );
}

/// A well-formed ping token (64 lowercase hex characters).
fn new_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

/// A heartbeat created 120 s ago with a 30 s period and no grace, never pinged.
async fn overdue_heartbeat(pool: &sqlx::PgPool, project: Uuid) -> (Uuid, String) {
    let token = new_token();
    let id = sqlx::query_scalar(
        "insert into heartbeats (project_id, name, token, period_seconds, grace_seconds, created_at)
         values ($1, 'Nightly backup', $2, 30, 0, now() - interval '120 seconds') returning id",
    )
    .bind(project)
    .bind(&token)
    .fetch_one(pool)
    .await
    .unwrap();
    (id, token)
}

async fn heartbeat_status(pool: &sqlx::PgPool, id: Uuid) -> String {
    sqlx::query_scalar("select current_status from heartbeats where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn the_sweep_skips_heartbeats_of_a_project_in_maintenance(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let quiet = seed_project(&pool).await;
    let scoped = seed_project(&pool).await;
    let ended = seed_project(&pool).await;
    let (covered, _) = overdue_heartbeat(&pool, quiet).await;
    let (uncovered, _) = overdue_heartbeat(&pool, scoped).await;
    let (after_window, _) = overdue_heartbeat(&pool, ended).await;
    let window = seed_window_now(&pool, quiet, None).await;
    // A window scoped to a monitor never covers heartbeats.
    let scoped_monitor = seed_http_monitor(&pool, scoped, "API", "http://127.0.0.1:9/", 1).await;
    seed_window_now(&pool, scoped, Some(scoped_monitor)).await;
    // Neither does one that is over.
    let over = seed_window_now(&pool, ended, None).await;
    end_window(&pool, over).await;

    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 2);

    assert_eq!(heartbeat_status(&pool, covered).await, "pending");
    assert_eq!(heartbeat_status(&pool, uncovered).await, "down");
    assert_eq!(heartbeat_status(&pool, after_window).await, "down");
    let incidents: i64 =
        sqlx::query_scalar("select count(*) from incidents where heartbeat_id = $1")
            .bind(covered)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(incidents, 0);

    let anchored: bool =
        sqlx::query_scalar("select outage_anchor_at is not null from heartbeats where id = $1")
            .bind(covered)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(anchored, "its deadline was moved to the sweep");

    // Right after the window it is not down: its deadline did not run out
    // during the window, it gets a full period (30 s) and grace (0 s).
    end_window(&pool, window).await;
    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 0);
    assert_eq!(heartbeat_status(&pool, covered).await, "pending");
    // 29 s later: still on time.
    pass_time(&pool, covered, 29).await;
    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 0);
    assert_eq!(heartbeat_status(&pool, covered).await, "pending");
    // Past period + grace without a ping: down.
    pass_time(&pool, covered, 2).await;
    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 1);
    assert_eq!(heartbeat_status(&pool, covered).await, "down");
}

/// As if `seconds` had passed for heartbeat `id`: every anchor of its
/// deadline moves that far into the past.
async fn pass_time(pool: &sqlx::PgPool, id: Uuid, seconds: i32) {
    sqlx::query(
        "update heartbeats set
             created_at = created_at - make_interval(secs => $2),
             last_ping_at = last_ping_at - make_interval(secs => $2),
             resumed_at = resumed_at - make_interval(secs => $2),
             outage_anchor_at = outage_anchor_at - make_interval(secs => $2)
         where id = $1",
    )
    .bind(id)
    .bind(f64::from(seconds))
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test]
async fn a_heartbeat_pinging_normally_during_a_window_is_unaffected(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let public = public_app(state.clone());
    let project = seed_project(&pool).await;
    let token = new_token();
    let id: Uuid = sqlx::query_scalar(
        "insert into heartbeats (project_id, name, token, period_seconds, grace_seconds, current_status, last_ping_at)
         values ($1, 'Cron', $2, 30, 0, 'up', now()) returning id",
    )
    .bind(project)
    .bind(&token)
    .fetch_one(&pool)
    .await
    .unwrap();
    let window = seed_window_now(&pool, project, None).await;

    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 0);
    let (status, _) = call(
        &public,
        "POST",
        &format!("/api/v1/heartbeat/{token}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 0);
    let (current, anchored): (String, bool) = sqlx::query_as(
        "select current_status, outage_anchor_at is not null from heartbeats where id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (current.as_str(), anchored),
        ("up", false),
        "on time: no re-anchoring"
    );

    end_window(&pool, window).await;
    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 0);
    let (status, _) = call(
        &public,
        "POST",
        &format!("/api/v1/heartbeat/{token}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(heartbeat_status(&pool, id).await, "up");
    // Its deadline runs from its last ping as always: 31 s without one, down.
    pass_time(&pool, id, 31).await;
    assert_eq!(sweep_heartbeats(&state).await.unwrap(), 1);
}

#[sqlx::test]
async fn a_ping_inside_a_window_is_recorded_but_resolves_nothing(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let public = public_app(state.clone());
    let project = seed_project(&pool).await;
    let recipient = unique_email("oncall");
    let user = seed_user(&pool, &recipient, "member").await;
    add_member(&pool, project, user, "viewer").await;
    let (id, token) = overdue_heartbeat(&pool, project).await;
    sweep_heartbeats(&state).await.unwrap();
    let window = seed_window_now(&pool, project, None).await;

    let (status, _) = call(
        &public,
        "POST",
        &format!("/api/v1/heartbeat/{token}"),
        None,
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    let (current, pinged): (String, bool) = sqlx::query_as(
        "select current_status, last_ping_at is not null from heartbeats where id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((current.as_str(), pinged), ("down", true));
    let open: i64 = sqlx::query_scalar(
        "select count(*) from incidents where heartbeat_id = $1 and status = 'open'",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(open, 1);
    assert_eq!(
        settled_mail(&recipient, 1).await.len(),
        1,
        "only the DOWN email, no recovery"
    );

    // After the window, a ping resolves it as usual.
    end_window(&pool, window).await;
    let (status, _) = call(
        &public,
        "GET",
        &format!("/api/v1/heartbeat/{token}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(heartbeat_status(&pool, id).await, "up");
}

#[sqlx::test]
async fn a_monitor_scoped_window_does_not_freeze_pings(pool: sqlx::PgPool) {
    let state = AppState::for_test(pool.clone());
    let public = public_app(state.clone());
    let project = seed_project(&pool).await;
    let (id, token) = overdue_heartbeat(&pool, project).await;
    let monitor_id = seed_http_monitor(&pool, project, "API", "http://127.0.0.1:9/", 1).await;
    seed_window_now(&pool, project, Some(monitor_id)).await;
    let (status, _) = call(
        &public,
        "POST",
        &format!("/api/v1/heartbeat/{token}"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(heartbeat_status(&pool, id).await, "up");
}

#[sqlx::test]
async fn reads_say_whether_a_monitor_or_heartbeat_is_in_maintenance(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let covered = seed_http_monitor(&pool, project, "Covered", "https://example.com", 2).await;
    let free = seed_http_monitor(&pool, project, "Free", "https://example.com", 2).await;
    let (heartbeat, _) = overdue_heartbeat(&pool, project).await;
    seed_window_now(&pool, project, Some(covered)).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;

    let (_, detail) = call(
        &app,
        "GET",
        &format!("/projects/{project}/monitors/{covered}"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(detail["in_maintenance"], true);
    let (_, list) = call(
        &app,
        "GET",
        &format!("/projects/{project}/monitors"),
        Some(&viewer),
        None,
    )
    .await;
    let flag = |id: Uuid| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id.to_string())
            .unwrap()["in_maintenance"]
            .clone()
    };
    assert_eq!((flag(covered), flag(free)), (json!(true), json!(false)));
    let (_, hb) = call(
        &app,
        "GET",
        &format!("/projects/{project}/heartbeats/{heartbeat}"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(
        hb["in_maintenance"], false,
        "a monitor-scoped window does not cover heartbeats"
    );
    assert_eq!(
        hb["token"],
        Value::Null,
        "viewers still never get the token"
    );

    seed_window_now(&pool, project, None).await;
    let (_, hbs) = call(
        &app,
        "GET",
        &format!("/projects/{project}/heartbeats"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(hbs[0]["in_maintenance"], true);
    assert_eq!(hbs[0]["token"], Value::Null);
    let (_, free_detail) = call(
        &app,
        "GET",
        &format!("/projects/{project}/monitors/{free}"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(
        free_detail["in_maintenance"], true,
        "a project-wide window covers every monitor"
    );

    // Writes answer with the flag too.
    let (status, patched) = call(
        &app,
        "PATCH",
        &format!("/projects/{project}/monitors/{free}"),
        Some(&member),
        Some(json!({ "name": "Free API" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["in_maintenance"], true);
    let (status, created) = call(
        &app,
        "POST",
        &format!("/projects/{project}/heartbeats"),
        Some(&member),
        Some(json!({ "name": "Cron", "period_seconds": 60 })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["in_maintenance"], true);
    assert!(created["token"].is_string(), "members get the token");
    let (status, rotated) = call(
        &app,
        "POST",
        &format!("/projects/{project}/heartbeats/{heartbeat}/rotate-token"),
        Some(&member),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rotated}");
    assert_eq!(rotated["in_maintenance"], true);
}

/// A member could otherwise create windows without end, and every monitor
/// list, `apply_outcome` and heartbeat sweep reads them (Phase 2 final review).
#[sqlx::test]
async fn a_project_holds_at_most_200_windows(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    for _ in 0..199 {
        seed_window_now(&pool, project, None).await;
    }
    let collection = format!("/projects/{project}/maintenance-windows");
    let (status, body) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(window_now("200th", None)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, body) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(window_now("201st", None)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        body["error"],
        "a project can have at most 200 maintenance windows: delete one first"
    );
    assert_eq!(window_count(&pool).await, 200);

    // Another project has its own allowance.
    let (_, _, elsewhere) = user_with_role(&app, &pool, other, Some("member")).await;
    let (status, _) = call(
        &app,
        "POST",
        &format!("/projects/{other}/maintenance-windows"),
        Some(&elsewhere),
        Some(window_now("first", None)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
}
