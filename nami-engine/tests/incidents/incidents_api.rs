// The incident API. Requires `docker compose up -d postgres`.
use crate::common;

use axum::http::StatusCode;
use common::{call, open_test_incident, seed_project, test_app, user_with_role};
use serde_json::{Value, json};
use uuid::Uuid;

fn url(project: Uuid, rest: &str) -> String {
    format!("/projects/{project}/incidents{rest}")
}

#[sqlx::test]
async fn members_open_manual_incidents_and_viewers_read_them(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (member_id, member_email, member) =
        user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;

    let (status, _) = call(
        &app,
        "POST",
        &url(project, ""),
        Some(&viewer),
        Some(json!({"title": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, created) = call(
        &app,
        "POST",
        &url(project, ""),
        Some(&member),
        Some(json!({"title": "  Checkout slow  ", "description": "p95 > 2s\r\nsince 10:00"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["title"], "Checkout slow");
    assert_eq!(created["cause"], "p95 > 2s\nsince 10:00", "CRLF from a textarea is stored as LF");
    assert_eq!(created["source"], "manual");
    assert_eq!(created["status"], "open");
    assert_eq!(created["severity"], "high");
    assert_eq!(created["created_by_email"], member_email);
    assert_eq!(created["events"][0]["kind"], "opened");
    assert_eq!(created["events"][0]["actor_id"], json!(member_id));

    let (status, listed) = call(&app, "GET", &url(project, ""), Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(listed["incidents"][0]["id"], created["id"]);
    assert_eq!(listed["next_cursor"], Value::Null);
}

#[sqlx::test]
async fn outsiders_get_403_and_foreign_ids_404(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (member_id, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let foreign = open_test_incident(&pool, other, member_id).await;

    let (status, _) = call(&app, "GET", &url(project, ""), Some(&outsider), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &app,
        "GET",
        &url(project, &format!("/{foreign}")),
        Some(&member),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &app,
        "POST",
        &url(project, &format!("/{foreign}/acknowledge")),
        Some(&member),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let foreign_severity: Uuid =
        sqlx::query_scalar("select id from incident_severities where project_id = $1 limit 1")
            .bind(other)
            .fetch_one(&pool)
            .await
            .unwrap();
    let (status, _) = call(
        &app,
        "POST",
        &url(project, ""),
        Some(&member),
        Some(json!({"title": "x", "severity_id": foreign_severity})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn ack_resolve_reopen_follow_the_lifecycle(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (member_id, member_email, member) =
        user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let id = open_test_incident(&pool, project, member_id).await;
    let action = |name: &str| url(project, &format!("/{id}/{name}"));

    let (status, _) = call(
        &app,
        "POST",
        &action("acknowledge"),
        Some(&viewer),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = call(
        &app,
        "POST",
        &action("acknowledge"),
        Some(&member),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (
            body["status"].as_str(),
            body["acknowledged_by_email"].as_str()
        ),
        (Some("acknowledged"), Some(member_email.as_str()))
    );
    let (status, _) = call(
        &app,
        "POST",
        &action("acknowledge"),
        Some(&member),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(&app, "POST", &action("reopen"), Some(&member), Some(json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, body) =
        call(&app, "POST", &action("resolve"), Some(&member), Some(json!({}))).await;
    assert_eq!(
        (status, body["status"].as_str()),
        (StatusCode::OK, Some("resolved"))
    );
    let (status, _) = call(&app, "POST", &action("resolve"), Some(&member), Some(json!({}))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, body) =
        call(&app, "POST", &action("reopen"), Some(&member), Some(json!({}))).await;
    assert_eq!(
        (status, body["status"].as_str()),
        (StatusCode::OK, Some("open"))
    );
    let kinds: Vec<&str> = body["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["opened", "acknowledged", "resolved", "reopened"]);
}

#[sqlx::test]
async fn comments_postmortems_and_severity_changes_are_capped_and_logged(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (member_id, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let id = open_test_incident(&pool, project, member_id).await;

    let (status, event) = call(
        &app,
        "POST",
        &url(project, &format!("/{id}/comments")),
        Some(&member),
        Some(json!({"body": "rolled back\r\nwatching"})),
    )
    .await;
    assert_eq!(
        (status, event["kind"].as_str()),
        (StatusCode::CREATED, Some("comment"))
    );
    assert_eq!(event["message"], "rolled back\nwatching");
    for bad in [
        json!({"body": ""}),
        json!({"body": "x".repeat(10_241)}),
        json!({"body": "a\u{1b}[31m"}),
    ] {
        let (status, _) = call(
            &app,
            "POST",
            &url(project, &format!("/{id}/comments")),
            Some(&member),
            Some(bad),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    let (status, body) = call(
        &app,
        "PUT",
        &url(project, &format!("/{id}/postmortem")),
        Some(&member),
        Some(json!({"postmortem": "# Cause\r\n<script>alert(1)</script>"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["postmortem"], "# Cause\n<script>alert(1)</script>",
        "stored verbatim; the UI escapes it"
    );
    let (status, _) = call(
        &app,
        "PUT",
        &url(project, &format!("/{id}/postmortem")),
        Some(&member),
        Some(json!({"postmortem": "x".repeat(102_401)})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let critical: Uuid = sqlx::query_scalar(
        "select id from incident_severities where project_id = $1 and name = 'critical'",
    )
    .bind(project)
    .fetch_one(&pool)
    .await
    .unwrap();
    let (status, body) = call(
        &app,
        "PATCH",
        &url(project, &format!("/{id}")),
        Some(&member),
        Some(json!({"severity_id": critical})),
    )
    .await;
    assert_eq!(
        (status, body["severity"].as_str()),
        (StatusCode::OK, Some("critical"))
    );
    let last = body["events"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["kind"], "severity_changed");
    assert_eq!(last["metadata"], json!({"from": "high", "to": "critical"}));
}

#[sqlx::test]
async fn titles_are_one_line_and_capped(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    for title in ["", "   ", "a\nb", "a\u{202e}b", &"x".repeat(256)] {
        let (status, _) = call(
            &app,
            "POST",
            &url(project, ""),
            Some(&member),
            Some(json!({"title": title})),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{title:?}");
    }
    let (status, _) = call(
        &app,
        "POST",
        &url(project, ""),
        Some(&member),
        Some(json!({"title": "x", "extra": 1})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unknown fields are refused"
    );
}

#[sqlx::test]
async fn the_list_filters_by_status_and_source_and_pages_without_gaps(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (member_id, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let mut ids = Vec::new();
    for _ in 0..5 {
        ids.push(open_test_incident(&pool, project, member_id).await);
    }
    // Same started_at for all five: paging must still return each exactly once.
    sqlx::query("update incidents set started_at = '2026-10-19T00:00:00Z' where project_id = $1")
        .bind(project)
        .execute(&pool)
        .await
        .unwrap();
    call(
        &app,
        "POST",
        &url(project, &format!("/{}/acknowledge", ids[0])),
        Some(&member),
        Some(json!({})),
    )
    .await;

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let q = match &cursor {
            Some(c) => format!("?limit=2&cursor={c}"),
            None => "?limit=2".into(),
        };
        let (status, page) = call(&app, "GET", &url(project, &q), Some(&member), None).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        seen.extend(
            page["incidents"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["id"].as_str().unwrap().to_string()),
        );
        match page["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
    }
    seen.sort();
    let mut expected: Vec<String> = ids.iter().map(Uuid::to_string).collect();
    expected.sort();
    assert_eq!(seen, expected);

    let (_, acked) = call(
        &app,
        "GET",
        &url(project, "?status=acknowledged"),
        Some(&member),
        None,
    )
    .await;
    assert_eq!(acked["incidents"].as_array().unwrap().len(), 1);
    let (_, unresolved) = call(
        &app,
        "GET",
        &url(project, "?status=unresolved&source=manual"),
        Some(&member),
        None,
    )
    .await;
    assert_eq!(unresolved["incidents"].as_array().unwrap().len(), 5);
    for bad in [
        "?status=bogus",
        "?source=x",
        "?limit=0",
        "?limit=101",
        "?cursor=%%%",
    ] {
        let (status, _) = call(&app, "GET", &url(project, bad), Some(&member), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
}

#[sqlx::test]
async fn severities_are_listed_by_rank_for_viewers(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (status, body) = call(
        &app,
        "GET",
        &format!("/projects/{project}/severities"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["critical", "high", "low"]);
}

#[sqlx::test]
async fn text_caps_and_the_page_size_accept_their_bound(pool: sqlx::PgPool) {
    use nami_engine::incidents::model::{
        MAX_LIST_LIMIT, MAX_POSTMORTEM_BYTES, MAX_TEXT_BYTES, MAX_TITLE_CHARS,
    };
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;

    let (status, created) = call(
        &app,
        "POST",
        &url(project, ""),
        Some(&member),
        Some(json!({"title": "\u{e9}".repeat(MAX_TITLE_CHARS), "description": "x".repeat(MAX_TEXT_BYTES)})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let (status, _) = call(
        &app,
        "POST",
        &url(project, ""),
        Some(&member),
        Some(json!({"title": "x", "description": "x".repeat(MAX_TEXT_BYTES + 1)})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let id = created["id"].as_str().unwrap();
    let (status, _) = call(
        &app,
        "POST",
        &url(project, &format!("/{id}/comments")),
        Some(&member),
        Some(json!({"body": "x".repeat(MAX_TEXT_BYTES)})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = call(
        &app,
        "PUT",
        &url(project, &format!("/{id}/postmortem")),
        Some(&member),
        Some(json!({"postmortem": "x".repeat(MAX_POSTMORTEM_BYTES)})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["postmortem"].as_str().map(str::len),
        Some(MAX_POSTMORTEM_BYTES)
    );
    let (status, body) = call(
        &app,
        "PUT",
        &url(project, &format!("/{id}/postmortem")),
        Some(&member),
        Some(json!({"postmortem": null})),
    )
    .await;
    assert_eq!((status, &body["postmortem"]), (StatusCode::OK, &Value::Null));
    let (status, _) = call(
        &app,
        "GET",
        &url(project, &format!("?limit={MAX_LIST_LIMIT}")),
        Some(&member),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[sqlx::test]
async fn every_write_needs_a_member_and_an_incident_of_the_path_project(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (member_id, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let id = open_test_incident(&pool, project, member_id).await;
    let foreign = open_test_incident(&pool, other, member_id).await;
    let severity: Uuid = sqlx::query_scalar(
        "select id from incident_severities where project_id = $1 and name = 'critical'",
    )
    .bind(project)
    .fetch_one(&pool)
    .await
    .unwrap();

    // Valid bodies, so the role check (not the body extractor) decides.
    let writes = |incident: Uuid| {
        [
            ("POST", format!("/{incident}/resolve"), json!({})),
            ("POST", format!("/{incident}/reopen"), json!({})),
            ("POST", format!("/{incident}/comments"), json!({"body": "x"})),
            ("PUT", format!("/{incident}/postmortem"), json!({"postmortem": "x"})),
            ("PATCH", format!("/{incident}"), json!({"severity_id": severity})),
        ]
    };
    for (method, rest, body) in writes(id) {
        let (status, _) = call(&app, method, &url(project, &rest), Some(&viewer), Some(body)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "viewer {method} {rest}");
    }
    for (method, rest, body) in writes(foreign) {
        let (status, _) = call(&app, method, &url(project, &rest), Some(&member), Some(body)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "member {method} {rest}");
    }

    let (status, _) = call(&app, "GET", &url(project, &format!("/{id}")), Some(&outsider), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &app,
        "GET",
        &format!("/projects/{project}/severities"),
        Some(&outsider),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let foreign_severity: Uuid =
        sqlx::query_scalar("select id from incident_severities where project_id = $1 limit 1")
            .bind(other)
            .fetch_one(&pool)
            .await
            .unwrap();
    let (status, _) = call(
        &app,
        "PATCH",
        &url(project, &format!("/{id}")),
        Some(&member),
        Some(json!({"severity_id": foreign_severity})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
