// The source pipeline on HTTP ingestion. Requires
// `docker compose up -d postgres clickhouse`.
mod common;

use axum::http::StatusCode;
use common::{
    app, call,
    logs::{post_json, seed_source, stored_rows, unreachable_pool},
    public_app, seed_project, state_with_clickhouse, user_with_role,
};
use serde_json::{Value, json};

async fn set_rules(
    app: &axum::Router,
    cookie: &axum::http::HeaderValue,
    project: uuid::Uuid,
    source: uuid::Uuid,
    rules: Value,
) {
    let uri = format!("/projects/{project}/sources/{source}/pipeline");
    let (status, body) = call(
        app,
        "PUT",
        &uri,
        Some(cookie),
        Some(json!({ "rules": rules })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// Writes rules around the API (as a future incompatible change could leave
/// them): this one no longer compiles.
async fn store_broken_rules(pool: &sqlx::PgPool, source: uuid::Uuid) {
    sqlx::query(
        "update sources set pipeline = '[{\"type\":\"redact\",\"regex\":\"(\"}]'::jsonb,
                updated_at = clock_timestamp() where id = $1",
    )
    .bind(source)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test]
async fn a_redacted_value_never_reaches_clickhouse(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let (source, token) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    set_rules(
        &app,
        &member,
        project,
        source,
        json!([{"type":"redact","keys":["password","api_key"],"regex":"sk_live_[A-Za-z0-9]+"}]),
    )
    .await;

    let (status, body) = post_json(
        &public,
        &token,
        json!({
            "message": "charging with sk_live_ABC123 for 'sk_live_QUOTED9'",
            "PASSWORD": "hunter2",
            "deep": { "Api_Key": "k-1", "list": [ { "password": "p" }, "sk_live_INARRAY" ] }
        }),
    )
    .await;
    assert_eq!(
        (status, body),
        (StatusCode::ACCEPTED, json!({ "accepted": 1 }))
    );
    let rows = stored_rows(&ch, project).await;
    let row = &rows[0];
    let text = row.to_string();
    for secret in [
        "sk_live_ABC123",
        "sk_live_QUOTED9",
        "hunter2",
        "k-1",
        "sk_live_INARRAY",
        "\\\"p\\\"",
    ] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
    assert_eq!(row["message"], "charging with [REDACTED] for '[REDACTED]'");
    let pattern = row["pattern"].as_str().unwrap();
    assert!(!pattern.contains("sk_live"), "{pattern}");
    assert!(
        pattern.contains("[REDACTED]"),
        "the pattern is of the redacted message: {pattern}"
    );
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn drops_and_samples_are_counted_and_not_accepted(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let (source, token) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    set_rules(
        &app,
        &member,
        project,
        source,
        json!([
            {"type":"drop","match":{"field":"level","equals":"debug"}},
            {"type":"sample","n":2}
        ]),
    )
    .await;
    let events: Vec<Value> = (0..4)
        .map(|n| json!({ "message": format!("m{n}") }))
        .chain([json!({ "level": "debug", "message": "noise" })])
        .collect();
    let (status, body) = post_json(&public, &token, json!(events)).await;
    assert_eq!(
        (status, body),
        (StatusCode::ACCEPTED, json!({ "accepted": 2 }))
    );
    assert_eq!(stored_rows(&ch, project).await.len(), 2);
    // A request the pipeline leaves nothing of sends no insert, still counted.
    let (status, body) =
        post_json(&public, &token, json!({ "level": "debug", "message": "x" })).await;
    assert_eq!(
        (status, body),
        (StatusCode::ACCEPTED, json!({ "accepted": 0 }))
    );
    let today = chrono::Utc::now().date_naive();
    let pending = logs.usage.pending(source, today);
    assert_eq!(
        (
            pending.events,
            pending.events_dropped,
            pending.events_sampled_out
        ),
        (2, 2, 2)
    );
    // And they reach `source_usage_daily`.
    logs.usage.flush(&pool).await.unwrap();
    let flushed: (i64, i64, i64) = sqlx::query_as(
        "select events, events_dropped, events_sampled_out from source_usage_daily
         where source_id = $1 and day = $2",
    )
    .bind(source)
    .bind(today)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(flushed, (2, 2, 2));
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_pipeline_change_applies_to_the_next_request(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let (source, token) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    // Cached: no rules.
    post_json(&public, &token, json!({ "message": "secret-1" })).await;
    set_rules(
        &app,
        &member,
        project,
        source,
        json!([{"type":"redact","regex":"secret-\\d"}]),
    )
    .await;
    post_json(&public, &token, json!({ "message": "secret-2" })).await;
    set_rules(&app, &member, project, source, json!([])).await;
    post_json(&public, &token, json!({ "message": "secret-3" })).await;
    let mut messages: Vec<String> = stored_rows(&ch, project)
        .await
        .iter()
        .map(|r| r["message"].as_str().unwrap().to_string())
        .collect();
    messages.sort();
    assert_eq!(messages, ["[REDACTED]", "secret-1", "secret-3"]);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_stored_pipeline_that_does_not_compile_fails_closed(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let project = seed_project(&pool).await;
    let (source, token) = seed_source(&pool, project, "api").await;
    let (_, other_token) = seed_source(&pool, project, "other").await;
    // Cached first with no rules: the broken edit must still be seen.
    assert_eq!(
        post_json(&public, &token, json!({ "message": "before" }))
            .await
            .0,
        StatusCode::ACCEPTED
    );
    store_broken_rules(&pool, source).await;
    let (status, body) = post_json(&public, &token, json!({ "message": "password=hunter2" })).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(body, json!({ "error": "internal error" }));
    assert_eq!(
        post_json(&public, &other_token, json!({ "message": "fine" }))
            .await
            .0,
        StatusCode::ACCEPTED,
        "other sources keep ingesting"
    );
    let mut messages: Vec<String> = stored_rows(&ch, project)
        .await
        .iter()
        .map(|r| r["message"].as_str().unwrap().to_string())
        .collect();
    messages.sort();
    assert_eq!(messages, ["before", "fine"]);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_quota_counts_bytes_after_the_pipeline(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let (source, token) = seed_source(&pool, project, "api").await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    set_rules(
        &app,
        &member,
        project,
        source,
        json!([{"type":"remove_fields","paths":["blob"]}]),
    )
    .await;
    post_json(
        &public,
        &token,
        json!({ "message": "m", "blob": "x".repeat(10_000) }),
    )
    .await;
    let pending = logs.usage.pending(source, chrono::Utc::now().date_naive());
    assert_eq!(pending.bytes, 1 + 2, "message \"m\" plus fields \"{{}}\"");
    ch.drop_database().await.unwrap();
}

fn ingest_source(id: uuid::Uuid) -> nami_engine::models::source::IngestSource {
    nami_engine::models::source::IngestSource {
        id,
        project_id: uuid::Uuid::new_v4(),
        retention_days: 30,
        daily_byte_quota: None,
        updated_at: chrono::Utc::now(),
    }
}

#[sqlx::test]
async fn a_source_deleted_before_its_pipeline_is_read_fails_closed(pool: sqlx::PgPool) {
    let cache = nami_engine::logs::pipeline_cache::PipelineCache::default();
    let result = cache
        .for_source(&pool, &ingest_source(uuid::Uuid::new_v4()), None)
        .await;
    assert!(
        matches!(
            result,
            Err(nami_engine::logs::pipeline_cache::PipelineLoadError::Gone)
        ),
        "{result:?}"
    );
    assert!(cache.is_empty());
}

#[tokio::test]
async fn a_database_error_reading_the_pipeline_fails_closed() {
    let cache = nami_engine::logs::pipeline_cache::PipelineCache::default();
    let result = cache
        .for_source(
            &unreachable_pool(),
            &ingest_source(uuid::Uuid::new_v4()),
            None,
        )
        .await;
    assert!(
        matches!(
            result,
            Err(nami_engine::logs::pipeline_cache::PipelineLoadError::Database(_))
        ),
        "{result:?}"
    );
}
