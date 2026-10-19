// Saved log views (`/projects/{id}/log-views`). Requires `docker compose up -d postgres`.
use crate::common;

use axum::http::StatusCode;
use common::{call, logs::seed_source, seed_project, test_app, user_with_role};
use serde_json::json;

#[sqlx::test]
async fn members_manage_views_and_viewers_read_them(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let base = format!("/projects/{project}/log-views");
    let query = json!({"range":"24h","levels":["error"],"source_ids":[source],"columns":["user.id"]});

    let (status, created) = call(&app, "POST", &base, Some(&member), Some(json!({"name":"Errors","query":query}))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["query"], query);
    let id = created["id"].as_str().unwrap().to_string();
    let one = format!("{base}/{id}");

    let (status, list) = call(&app, "GET", &base, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(call(&app, "GET", &one, Some(&viewer), None).await.1["name"], "Errors");

    let (status, renamed) = call(&app, "PATCH", &one, Some(&member), Some(json!({"name":"5xx"}))).await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    assert_eq!((renamed["name"].clone(), renamed["query"].clone()), (json!("5xx"), query));

    for (method, body) in [("POST", Some(json!({"name":"x","query":{}}))), ("PATCH", Some(json!({"name":"y"}))), ("DELETE", None)] {
        let uri = if method == "POST" { &base } else { &one };
        assert_eq!(call(&app, method, uri, Some(&viewer), body).await.0, StatusCode::FORBIDDEN, "{method}");
    }
    assert_eq!(call(&app, "GET", &base, Some(&outsider), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "GET", &one, Some(&outsider), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "DELETE", &one, Some(&outsider), None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, "GET", &base, None, None).await.0, StatusCode::UNAUTHORIZED);

    assert_eq!(call(&app, "DELETE", &one, Some(&member), None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(call(&app, "GET", &one, Some(&member), None).await.0, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn a_view_of_another_project_is_not_found(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, theirs) = user_with_role(&app, &pool, other, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{other}/log-views"), Some(&theirs), Some(json!({"name":"Theirs","query":{}}))).await;
    let foreign = format!("/projects/{project}/log-views/{}", created["id"].as_str().unwrap());
    assert_eq!(call(&app, "GET", &foreign, Some(&member), None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&app, "PATCH", &foreign, Some(&member), Some(json!({"name":"mine"}))).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&app, "DELETE", &foreign, Some(&member), None).await.0, StatusCode::NOT_FOUND);
    // The member of `project` cannot reach `other`'s collection either.
    assert_eq!(call(&app, "GET", &format!("/projects/{other}/log-views"), Some(&member), None).await.0, StatusCode::FORBIDDEN);
    let name: String = sqlx::query_scalar("select name from log_views where project_id = $1").bind(other).fetch_one(&pool).await.unwrap();
    assert_eq!(name, "Theirs");
}

#[sqlx::test]
async fn invalid_views_are_400_and_conflicts_409(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (foreign_source, _) = seed_source(&pool, other, "theirs").await;
    let (user, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let base = format!("/projects/{project}/log-views");
    for body in [
        json!({"name":"", "query":{}}),
        json!({"name":"a\u{0007}b", "query":{}}),
        json!({"name":"a\u{202E}b", "query":{}}),
        json!({"name":"a\u{200B}b", "query":{}}),
        json!({"name":"x", "query":{"range":"2h"}}),
        json!({"name":"x", "query":{"source_ids":[foreign_source]}}),
        json!({"name":"x", "query":{"filters":[{"path":"a","op":"regex","value":"("}]}}),
    ] {
        let (status, error) = call(&app, "POST", &base, Some(&member), Some(body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {error}");
    }
    let (status, _) = call(&app, "POST", &base, Some(&member), Some(json!({"name":"x","query":{},"extra":1}))).await;
    assert!(status.is_client_error());
    assert_eq!(call(&app, "POST", &base, Some(&member), Some(json!({"name":"Same","query":{}}))).await.0, StatusCode::CREATED);
    assert_eq!(call(&app, "POST", &base, Some(&member), Some(json!({"name":"Same","query":{}}))).await.0, StatusCode::CONFLICT);
    // Rename onto an existing name, and an invalid PATCH query.
    let (_, other_view) = call(&app, "POST", &base, Some(&member), Some(json!({"name":"Other","query":{}}))).await;
    let other_uri = format!("{base}/{}", other_view["id"].as_str().unwrap());
    assert_eq!(call(&app, "PATCH", &other_uri, Some(&member), Some(json!({"name":"Same"}))).await.0, StatusCode::CONFLICT);
    assert_eq!(call(&app, "PATCH", &other_uri, Some(&member), Some(json!({"query":{"source_ids":[foreign_source]}}))).await.0, StatusCode::BAD_REQUEST);
    sqlx::query("insert into log_views (project_id, name, created_by) select $1, 'v' || n, $2 from generate_series(1, 198) n")
        .bind(project).bind(user).execute(&pool).await.unwrap();
    // 2 + 198 = 200 views: one more is refused.
    let (status, body) = call(&app, "POST", &base, Some(&member), Some(json!({"name":"one too many","query":{}}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[sqlx::test]
async fn concurrent_creates_cannot_exceed_the_cap(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (user, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    sqlx::query("insert into log_views (project_id, name, created_by) select $1, 'v' || n, $2 from generate_series(1, 195) n")
        .bind(project).bind(user).execute(&pool).await.unwrap();
    let base = format!("/projects/{project}/log-views");
    let mut tasks = Vec::new();
    for n in 0..12 {
        let (app, member, base) = (app.clone(), member.clone(), base.clone());
        tasks.push(tokio::spawn(async move {
            call(&app, "POST", &base, Some(&member), Some(json!({"name": format!("race{n}"), "query": {}}))).await.0
        }));
    }
    let mut created = 0;
    for task in tasks {
        match task.await.unwrap() {
            StatusCode::CREATED => created += 1,
            StatusCode::CONFLICT => {}
            other => panic!("unexpected {other}"),
        }
    }
    assert_eq!(created, 5);
    let total: i64 = sqlx::query_scalar("select count(*) from log_views where project_id = $1")
        .bind(project).fetch_one(&pool).await.unwrap();
    assert_eq!(total, 200);
}

#[sqlx::test]
async fn writes_must_be_json(pool: sqlx::PgPool) {
    use axum::{body::Body, http::{Request, header}};
    use tower::ServiceExt;
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let request = Request::builder()
        .method("POST")
        .uri(format!("/projects/{project}/log-views"))
        .header(header::COOKIE, &member)
        .header(header::CONTENT_TYPE, "text/plain")
        .body(Body::from(r#"{"name":"x","query":{}}"#))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    let count: i64 = sqlx::query_scalar("select count(*) from log_views").fetch_one(&pool).await.unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn a_relative_view_stays_relative_and_a_deleted_source_is_kept(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/log-views"), Some(&member),
        Some(json!({"name":"Last hour","query":{"range":"1h","source_ids":[source]}}))).await;
    sqlx::query("delete from sources where id = $1").bind(source).execute(&pool).await.unwrap();
    let (status, view) = call(&app, "GET", &format!("/projects/{project}/log-views/{}", created["id"].as_str().unwrap()), Some(&member), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["query"], json!({"range":"1h","source_ids":[source]}), "no absolute window was frozen in");
}
