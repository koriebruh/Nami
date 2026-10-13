mod common;

use axum::http::StatusCode;
use common::{call, seed_http_monitor, seed_project, test_app, user_with_role};
use serde_json::{Value, json};
use uuid::Uuid;

fn http_monitor(name: &str) -> Value {
    json!({ "name": name, "check_type": "http", "url": "https://example.com/health" })
}

#[sqlx::test]
async fn member_creates_reads_and_updates_admin_deletes(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let collection = format!("/projects/{project}/monitors");

    let (status, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(http_monitor("API")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["project_id"], project.to_string());
    assert_eq!(created["http_method"], "GET", "defaults are applied");
    assert_eq!(created["expected_status_min"], 200);
    assert_eq!(created["expected_status_max"], 399);
    assert_eq!(created["interval_seconds"], 60);
    assert_eq!(created["timeout_seconds"], 10);
    assert_eq!(created["confirmation_threshold"], 2);
    assert_eq!(created["current_status"], "pending");
    let id = created["id"].as_str().unwrap().to_string();
    let item = format!("{collection}/{id}");

    let (status, list) = call(&app, "GET", &collection, Some(&member), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], id.as_str());

    let (status, patched) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "name": "API v2", "paused": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["name"], "API v2");
    assert_eq!(patched["paused"], true);
    assert_eq!(
        patched["url"], "https://example.com/health",
        "fields absent from a PATCH are kept"
    );

    let (status, _) = call(&app, "DELETE", &item, Some(&member), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "members cannot delete");

    let (status, body) = call(&app, "DELETE", &item, Some(&admin), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "deleted": true }));
    let (status, _) = call(&app, "GET", &item, Some(&admin), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn viewer_can_read_but_not_write(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let collection = format!("/projects/{project}/monitors");
    let item = format!("{collection}/{monitor}");

    assert_eq!(
        call(&app, "GET", &collection, Some(&viewer), None).await.0,
        StatusCode::OK
    );
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
            Some(http_monitor("x"))
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
            Some(json!({ "paused": true }))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "DELETE", &item, Some(&viewer), None).await.0,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test]
async fn non_member_gets_403_never_404(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let collection = format!("/projects/{project}/monitors");
    let existing = format!("{collection}/{monitor}");
    let missing = format!("{collection}/{}", Uuid::new_v4());

    assert_eq!(
        call(&app, "GET", &collection, Some(&outsider), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &collection,
            Some(&outsider),
            Some(http_monitor("x"))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for uri in [&existing, &missing] {
        assert_eq!(
            call(&app, "GET", uri, Some(&outsider), None).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(
                &app,
                "PATCH",
                uri,
                Some(&outsider),
                Some(json!({ "paused": true }))
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&app, "DELETE", uri, Some(&outsider), None).await.0,
            StatusCode::FORBIDDEN
        );
    }
}

#[sqlx::test]
async fn monitor_of_another_project_is_404_inside_an_accessible_project(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project_a = seed_project(&pool).await;
    let project_b = seed_project(&pool).await;
    let foreign =
        seed_http_monitor(&pool, project_b, "B's monitor", "https://example.com", 2).await;
    let (_, _, admin_a) = user_with_role(&app, &pool, project_a, Some("admin")).await;
    let uri = format!("/projects/{project_a}/monitors/{foreign}");

    assert_eq!(
        call(&app, "GET", &uri, Some(&admin_a), None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "PATCH",
            &uri,
            Some(&admin_a),
            Some(json!({ "name": "hijacked" }))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "DELETE", &uri, Some(&admin_a), None).await.0,
        StatusCode::NOT_FOUND
    );
    let (_, list) = call(
        &app,
        "GET",
        &format!("/projects/{project_a}/monitors"),
        Some(&admin_a),
        None,
    )
    .await;
    assert_eq!(list, json!([]));

    let (name, project): (String, Uuid) =
        sqlx::query_as("select name, project_id from monitors where id = $1")
            .bind(foreign)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (name.as_str(), project),
        ("B's monitor", project_b),
        "B's monitor is untouched"
    );
}

/// Review Focus: a keyword monitor with HEAD (no body to search) is
/// rejected instead of silently reporting DOWN forever.
#[sqlx::test]
async fn invalid_configs_are_rejected_with_400_and_nothing_is_written(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let url = "https://example.com";
    let cases = [
        (
            json!({ "name": "x", "check_type": "icmp", "url": url }),
            "check_type",
        ),
        (
            json!({ "name": "  ", "check_type": "http", "url": url }),
            "name",
        ),
        (
            json!({ "name": "a\r\nBcc: x@evil.test", "check_type": "http", "url": url }),
            "control",
        ),
        (json!({ "name": "x", "check_type": "http" }), "url"),
        (
            json!({ "name": "x", "check_type": "http", "url": "ftp://example.com" }),
            "url",
        ),
        (
            json!({ "name": "x", "check_type": "http", "url": "not a url" }),
            "url",
        ),
        (
            json!({ "name": "x", "check_type": "http", "url": url, "http_method": "POST" }),
            "http_method",
        ),
        (
            json!({ "name": "x", "check_type": "keyword", "url": url }),
            "keyword",
        ),
        (
            json!({ "name": "x", "check_type": "keyword", "url": url, "keyword": "   " }),
            "keyword",
        ),
        (
            json!({ "name": "x", "check_type": "keyword", "url": url, "keyword": "ok", "http_method": "HEAD" }),
            "GET",
        ),
        (
            json!({ "name": "x", "check_type": "tcp", "port": 5432 }),
            "host",
        ),
        (
            json!({ "name": "x", "check_type": "tcp", "host": "db" }),
            "port",
        ),
        (
            json!({ "name": "x", "check_type": "tcp", "host": "db", "port": 0 }),
            "port",
        ),
        (
            json!({ "name": "x", "check_type": "tcp", "host": "db", "port": 65536 }),
            "port",
        ),
        (
            json!({ "name": "x", "check_type": "http", "url": url, "interval_seconds": 29 }),
            "interval_seconds",
        ),
        (
            json!({ "name": "x", "check_type": "http", "url": url, "timeout_seconds": 0 }),
            "timeout_seconds",
        ),
        (
            json!({ "name": "x", "check_type": "http", "url": url, "timeout_seconds": 61 }),
            "timeout_seconds",
        ),
        (
            json!({ "name": "x", "check_type": "http", "url": url, "expected_status_min": 400, "expected_status_max": 300 }),
            "expected_status_min",
        ),
        (
            json!({ "name": "x", "check_type": "http", "url": url, "expected_status_max": 600 }),
            "between 100 and 599",
        ),
        (
            json!({ "name": "x", "check_type": "http", "url": url, "confirmation_threshold": 0 }),
            "confirmation_threshold",
        ),
        (
            json!({ "name": "x", "check_type": "http", "url": url, "confirmation_threshold": 11 }),
            "confirmation_threshold",
        ),
    ];
    for (body, fragment) in cases {
        let (status, json) = call(
            &app,
            "POST",
            &format!("/projects/{project}/monitors"),
            Some(&member),
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} returned {json}");
        let message = json["error"].as_str().unwrap_or_default();
        assert!(
            message.contains(fragment),
            "{body}: error {message:?} should mention {fragment:?}"
        );
    }
    let count: i64 = sqlx::query_scalar("select count(*) from monitors")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "no invalid monitor was written");
}

#[sqlx::test]
async fn invalid_patch_is_rejected_and_leaves_the_monitor_unchanged(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let uri = format!("/projects/{project}/monitors/{monitor}");

    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        Some(&member),
        Some(json!({ "check_type": "tcp" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"].as_str().unwrap().contains("host"));
    let (status, _) = call(
        &app,
        "PATCH",
        &uri,
        Some(&member),
        Some(json!({ "interval_seconds": 10 })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (_, current) = call(&app, "GET", &uri, Some(&member), None).await;
    assert_eq!(current["check_type"], "http");
    assert_eq!(current["url"], "https://example.com");
    assert_eq!(current["interval_seconds"], 60);
}

#[sqlx::test]
async fn switching_check_type_clears_fields_of_the_old_type(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "DB", "https://example.com", 2).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let uri = format!("/projects/{project}/monitors/{monitor}");

    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        Some(&member),
        Some(json!({ "check_type": "tcp", "host": " db.internal ", "port": 5432 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["host"], "db.internal", "host is trimmed");
    assert_eq!(body["port"], 5432);
    assert_eq!(body["url"], Value::Null, "the http url no longer applies");
}

#[sqlx::test]
async fn editing_check_config_reschedules_now_but_renaming_does_not(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let uri = format!("/projects/{project}/monitors/{monitor}");
    sqlx::query("update monitors set next_check_at = now() + interval '1 hour' where id = $1")
        .bind(monitor)
        .execute(&pool)
        .await
        .unwrap();
    let due_now = || async {
        sqlx::query_scalar::<_, bool>("select next_check_at <= now() from monitors where id = $1")
            .bind(monitor)
            .fetch_one(&pool)
            .await
            .unwrap()
    };

    call(
        &app,
        "PATCH",
        &uri,
        Some(&member),
        Some(json!({ "name": "renamed" })),
    )
    .await;
    assert!(!due_now().await, "a rename keeps the schedule");

    let (status, _) = call(
        &app,
        "PATCH",
        &uri,
        Some(&member),
        Some(json!({ "url": "https://example.org/" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(due_now().await, "a new url is checked on the next tick");
}

#[sqlx::test]
async fn server_owned_fields_in_the_body_are_ignored(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other_project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;

    let mut body = http_monitor("API");
    body["project_id"] = json!(other_project);
    body["current_status"] = json!("up");
    body["consecutive_failures"] = json!(7);
    let (status, created) = call(
        &app,
        "POST",
        &format!("/projects/{project}/monitors"),
        Some(&member),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["project_id"], project.to_string());
    assert_eq!(created["current_status"], "pending");

    let uri = format!(
        "/projects/{project}/monitors/{}",
        created["id"].as_str().unwrap()
    );
    let (status, patched) = call(
        &app,
        "PATCH",
        &uri,
        Some(&member),
        Some(json!({ "name": "x", "project_id": other_project, "current_status": "down", "consecutive_failures": 9 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(patched["project_id"], project.to_string());
    assert_eq!(patched["current_status"], "pending");
    assert_eq!(patched["consecutive_failures"], 0);
}
