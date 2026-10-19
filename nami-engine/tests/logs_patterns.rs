// Top message patterns (`GET /projects/{id}/logs/patterns`).
// Requires `docker compose up -d postgres clickhouse`.
mod common;

use axum::http::StatusCode;
use chrono::{Duration, SecondsFormat, Utc};
use common::{
    app, call,
    logs::{post_json, seed_source},
    public_app, seed_project, state_with_clickhouse, user_with_role,
};
use serde_json::{Value, json};

fn encode(text: &str) -> String {
    form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

fn window() -> String {
    let to = Utc::now() + Duration::minutes(1);
    let from = to - Duration::hours(1);
    format!(
        "from={}&to={}",
        encode(&from.to_rfc3339_opts(SecondsFormat::Millis, true)),
        encode(&to.to_rfc3339_opts(SecondsFormat::Millis, true))
    )
}

#[sqlx::test]
async fn patterns_are_counted_ranked_and_confined_to_the_project(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (foreign_source, foreign) = seed_source(&pool, other, "theirs").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;

    let events: Vec<Value> = (0..5)
        .map(|n| json!({ "message": format!("user {n} logged in from 10.0.0.{n}") }))
        .chain((0..2).map(|n| json!({ "message": format!("job {n} failed"), "level": "error" })))
        .collect();
    assert_eq!(
        post_json(&public, &token, json!(events)).await.0,
        StatusCode::ACCEPTED
    );
    for _ in 0..9 {
        post_json(&public, &foreign, json!({ "message": "theirs 1 only" })).await;
    }

    let uri = format!("/projects/{project}/logs/patterns?{}", window());
    let (status, body) = call(&app, "GET", &uri, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let patterns = body["patterns"].as_array().unwrap();
    let summary: Vec<(String, u64)> = patterns
        .iter()
        .map(|p| {
            (
                p["pattern"].as_str().unwrap().to_string(),
                p["count"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("user <num> logged in from <ip>".to_string(), 5),
            ("job <num> failed".to_string(), 2),
        ],
        "most frequent first, other projects never counted"
    );
    let first = &patterns[0];
    assert!(
        first["pattern_hash"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .is_ok()
    );
    assert!(first["first_seen"].as_str().unwrap() <= first["last_seen"].as_str().unwrap());

    // The hash filters search to exactly those rows.
    let hash = first["pattern_hash"].as_str().unwrap();
    let (_, page) = call(
        &app,
        "GET",
        &format!("/projects/{project}/logs?{}&pattern_hash={hash}", window()),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(page["logs"].as_array().unwrap().len(), 5);

    // Filters narrow the patterns; another project's source matches nothing.
    let (_, errors) = call(
        &app,
        "GET",
        &format!("{uri}&level=error"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(errors["patterns"].as_array().unwrap().len(), 1);
    let (_, foreign_rows) = call(
        &app,
        "GET",
        &format!("{uri}&source_id={foreign_source}"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(foreign_rows["patterns"], json!([]));
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn at_most_one_hundred_patterns_are_returned(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    // 120 distinct patterns: the word changes, so no placeholder merges them.
    let words =
        (0..120).map(|n| format!("event{}", char::from(b'a' + (n % 26) as u8)).repeat(1 + n / 26));
    let events: Vec<Value> = words
        .map(|w| json!({ "message": format!("{w} happened") }))
        .collect();
    post_json(&public, &token, json!(events)).await;
    let (_, body) = call(
        &app,
        "GET",
        &format!("/projects/{project}/logs/patterns?{}", window()),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(body["patterns"].as_array().unwrap().len(), 100);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn patterns_need_access_and_validate_like_search(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let base = format!("/projects/{project}/logs/patterns");
    assert_eq!(
        call(&app, "GET", &base, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(&app, "GET", &base, Some(&outsider), None).await.0,
        StatusCode::FORBIDDEN
    );
    for bad in [
        "from=2026-01-01T00:00:00Z&to=2026-03-01T00:00:00Z",
        "level=loud",
        "filter=%7B%22path%22%3A%22a%22%2C%22op%22%3A%22gt%22%2C%22value%22%3A%22x%22%7D",
    ] {
        let (status, body) = call(&app, "GET", &format!("{base}?{bad}"), Some(&viewer), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
    }
    // Injection text is a literal: no error, no rows.
    let evil = encode("'; DROP TABLE logs; -- {p0:String}");
    let (status, body) = call(
        &app,
        "GET",
        &format!("{base}?q={evil}"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!((status, body), (StatusCode::OK, json!({ "patterns": [] })));
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn patterns_share_the_per_user_query_slots(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let app = app(state);
    let project = seed_project(&pool).await;
    let (user_id, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let held: Vec<_> = (0..nami_engine::logs::search::MAX_QUERIES_PER_USER)
        .map(|_| nami_engine::logs::search::QueryPermit::acquire(&logs, user_id).unwrap())
        .collect();
    let uri = format!("/projects/{project}/logs/patterns");
    assert_eq!(
        call(&app, "GET", &uri, Some(&viewer), None).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );
    drop(held);
    assert_eq!(
        call(&app, "GET", &uri, Some(&viewer), None).await.0,
        StatusCode::OK
    );
    ch.drop_database().await.unwrap();
}
