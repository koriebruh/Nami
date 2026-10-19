// Pipeline editor API (`/projects/{id}/sources/{source_id}/pipeline`).
// Requires `docker compose up -d postgres clickhouse`.
use crate::common;

use axum::http::StatusCode;
use common::{
    app, call,
    logs::{seed_source, stored_rows},
    seed_project, state_with_clickhouse, test_app, user_with_role,
};
use serde_json::json;

#[sqlx::test]
async fn members_replace_the_rules_and_viewers_read_them(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let uri = format!("/projects/{project}/sources/{source}/pipeline");

    let (status, empty) = call(&app, "GET", &uri, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(empty["rules"], json!([]));
    // Parsed, not compared as text: chrono prints 0, 3 or 6 fraction digits.
    let at = |v: &serde_json::Value| {
        chrono::DateTime::parse_from_rfc3339(v["updated_at"].as_str().unwrap()).unwrap()
    };
    let before = at(&empty);

    let rules = json!([{"type":"redact","keys":["password"]},{"type":"sample","n":10}]);
    let (status, saved) = call(
        &app,
        "PUT",
        &uri,
        Some(&member),
        Some(json!({ "rules": rules })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["rules"], rules);
    assert!(at(&saved) > before, "updated_at is bumped");
    // Saving the same rules again still moves updated_at forward (R-P2-6).
    let (_, again) = call(
        &app,
        "PUT",
        &uri,
        Some(&member),
        Some(json!({ "rules": rules })),
    )
    .await;
    assert!(at(&again) > at(&saved));
    assert_eq!(
        call(&app, "GET", &uri, Some(&viewer), None).await.1["rules"],
        rules
    );

    let body = Some(json!({ "rules": [] }));
    assert_eq!(
        call(&app, "PUT", &uri, Some(&viewer), body.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "PUT", &uri, Some(&outsider), body.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "GET", &uri, Some(&outsider), None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "PUT", &uri, None, body).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[sqlx::test]
async fn a_source_of_another_project_is_not_found(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (foreign, _) = seed_source(&pool, other, "theirs").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let uri = format!("/projects/{project}/sources/{foreign}/pipeline");
    assert_eq!(
        call(&app, "GET", &uri, Some(&member), None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "PUT", &uri, Some(&member), Some(json!({"rules": []})))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    // The source is looked up before the rules are validated: invalid rules
    // for a foreign or unknown source are 404, not 400.
    let invalid = Some(json!({"rules": [{"type":"redact","regex":"("}]}));
    let unknown = format!(
        "/projects/{project}/sources/{}/pipeline",
        uuid::Uuid::new_v4()
    );
    for target in [&uri, &unknown] {
        let (status, body) = call(&app, "PUT", target, Some(&member), invalid.clone()).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{target}: {body}");
    }
    // The preview answers 404 for another project's source too.
    let preview = Some(json!({"rules": [], "events": [{"message":"m"}]}));
    assert_eq!(
        call(&app, "POST", &format!("{uri}/test"), Some(&member), preview)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let stored: serde_json::Value =
        sqlx::query_scalar("select pipeline from sources where id = $1")
            .bind(foreign)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        stored,
        json!([]),
        "never written through another project's path"
    );
}

#[sqlx::test]
async fn invalid_rules_are_400_naming_the_rule_and_nothing_is_stored(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let uri = format!("/projects/{project}/sources/{source}/pipeline");
    for (rules, needle) in [
        (
            json!([{"type":"sample","n":2},{"type":"redact","regex":"("}]),
            "rules[1]",
        ),
        (
            json!([{"type":"redact","regex":"a".repeat(257)}]),
            "rules[0]",
        ),
        (json!([{"type":"redact","regex":"a*"}]), "empty"),
        (json!(vec![json!({"type":"sample","n":2}); 51]), "rules[50]"),
        (json!({"type":"drop"}), "array"),
    ] {
        let (status, body) = call(
            &app,
            "PUT",
            &uri,
            Some(&member),
            Some(json!({ "rules": rules })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body["error"].as_str().unwrap().contains(needle), "{body}");
    }
    let (status, _) = call(
        &app,
        "PUT",
        &uri,
        Some(&member),
        Some(json!({ "rules": [], "extra": 1 })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unknown body keys are refused by the extractor"
    );
    assert_eq!(
        call(&app, "GET", &uri, Some(&member), None).await.1["rules"],
        json!([])
    );
}

#[sqlx::test]
async fn the_test_endpoint_previews_without_storing(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let uri = format!("/projects/{project}/sources/{source}/pipeline/test");
    let body = json!({
        "rules": [
            {"type":"drop","match":{"field":"level","equals":"debug"}},
            {"type":"redact","keys":["password"],"regex":"tok_[a-z0-9]+"},
            {"type":"sample","n":2,"match":{"message_regex":"^tick"}}
        ],
        "events": [
            {"level":"debug","message":"noise"},
            {"message":"login tok_abc123 for user 7","password":"hunter2"},
            {"message":"tick 1"},
            {"message":"tick 2"}
        ]
    });
    let (status, out) = call(&app, "POST", &uri, Some(&member), Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK, "{out}");
    let results = out["results"].as_array().unwrap();
    assert_eq!(results[0], json!({"outcome":"dropped"}));
    assert_eq!(results[1]["outcome"], "kept");
    assert_eq!(
        results[1]["event"]["message"],
        "login [REDACTED] for user 7"
    );
    assert_eq!(
        results[1]["event"]["fields"],
        json!({"password":"[REDACTED]"})
    );
    assert_eq!(
        results[1]["event"]["pattern"],
        "login [REDACTED] for user <num>"
    );
    assert_eq!(results[2]["outcome"], "kept");
    assert_eq!(results[3], json!({"outcome":"sampled_out"}));
    assert!(
        stored_rows(&ch, project).await.is_empty(),
        "the preview stores nothing"
    );

    assert_eq!(
        call(&app, "POST", &uri, Some(&viewer), Some(body)).await.0,
        StatusCode::FORBIDDEN
    );
    let many = json!({ "rules": [], "events": vec![json!({"message":"m"}); 101] });
    assert_eq!(
        call(&app, "POST", &uri, Some(&member), Some(many)).await.0,
        StatusCode::BAD_REQUEST
    );
    let not_object = json!({ "rules": [], "events": ["text"] });
    let (status, err) = call(&app, "POST", &uri, Some(&member), Some(not_object)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        err["error"].as_str().unwrap().contains("events[0]"),
        "{err}"
    );
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn every_write_to_a_source_moves_updated_at_forward(pool: sqlx::PgPool) {
    // R-P2-6: the pipeline cache is keyed on updated_at, so no write may move
    // it back, not even when the stored value is ahead of the clock.
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let ahead: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "update sources set updated_at = now() + interval '1 hour' where id = $1 returning updated_at",
    )
    .bind(source)
    .fetch_one(&pool)
    .await
    .unwrap();
    let at = |v: &serde_json::Value| {
        chrono::DateTime::parse_from_rfc3339(v["updated_at"].as_str().unwrap())
            .unwrap()
            .with_timezone(&chrono::Utc)
    };
    let item = format!("/projects/{project}/sources/{source}");
    let (status, patched) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({"name":"renamed"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert!(at(&patched) > ahead, "PATCH: {patched}");
    let (status, rotated) = call(
        &app,
        "POST",
        &format!("{item}/rotate"),
        Some(&member),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rotated}");
    assert!(at(&rotated) > at(&patched), "rotate: {rotated}");
    let (status, saved) = call(
        &app,
        "PUT",
        &format!("{item}/pipeline"),
        Some(&member),
        Some(json!({"rules": []})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert!(at(&saved) > at(&rotated), "pipeline: {saved}");
}

#[sqlx::test]
async fn errors_never_echo_a_pattern_and_writes_must_be_json(pool: sqlx::PgPool) {
    use axum::{body::Body, http::Request};
    use tower::ServiceExt;

    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let uri = format!("/projects/{project}/sources/{source}/pipeline");
    for rules in [
        json!([{"type":"redact","regex":"(?:tok_SECRETSHAPE)?"}]),
        json!([{"type":"redact","regex":"tok_SECRETSHAPE("}]),
        json!([{"type":"redact","regex":"tok_SECRETSHAPE\\w{200}\\w{50}"}]),
        json!([{"type":"redact","keys":"tok_SECRETSHAPE"}]),
    ] {
        let (status, body) = call(
            &app,
            "PUT",
            &uri,
            Some(&member),
            Some(json!({ "rules": rules })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(!body.to_string().contains("SECRETSHAPE"), "{body}");
    }
    let test_uri = format!("{uri}/test");
    let preview = Some(json!({"rules": [], "events": []}));
    assert_eq!(
        call(&app, "POST", &test_uri, Some(&outsider), preview.clone())
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "POST", &test_uri, None, preview).await.0,
        StatusCode::UNAUTHORIZED
    );

    let form = Request::builder()
        .method("PUT")
        .uri(&uri)
        .header("cookie", &member)
        .header("content-type", "text/plain")
        .body(Body::from(r#"{"rules":[]}"#))
        .unwrap();
    let response = app.clone().oneshot(form).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
}
