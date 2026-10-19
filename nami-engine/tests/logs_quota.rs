// Per-source ingestion stats (`source_usage_daily`) and the daily byte
// quota. Requires `docker compose up -d postgres clickhouse`.
mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use chrono::Utc;
use common::{
    app, call,
    logs::{
        post_json, post_logs, seed_source, seed_source_with, source_config, stored_rows,
        unreachable_pool,
    },
    public_app, seed_project, state_with_clickhouse, user_with_role,
};
use nami_engine::{
    core::state::AppState,
    logs::source::model::SourceConfig,
    logs::{
        LogsState,
        ingest::usage::{self, UsageCounters},
    },
};
use serde_json::{Value, json};
use uuid::Uuid;

async fn usage_row(pool: &sqlx::PgPool, source_id: Uuid) -> UsageCounters {
    sqlx::query_as(
        "select events, bytes, events_dropped, events_sampled_out, requests_rate_limited, requests_over_quota
         from source_usage_daily where source_id = $1 and day = $2",
    )
    .bind(source_id)
    .bind(Utc::now().date_naive())
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Bytes as ClickHouse stores them: UTF-8 length of message + fields.
async fn stored_bytes(ch: &nami_engine::core::clickhouse::ClickHouse, source_id: Uuid) -> i64 {
    let rows: Vec<Value> = ch
        .query_rows(
            "SELECT sum(length(message) + length(fields)) AS n FROM logs WHERE source_id = {s:UUID}",
            &[("s", source_id.to_string())],
        )
        .await
        .unwrap();
    rows[0]["n"].as_i64().unwrap()
}

#[sqlx::test]
async fn accepted_events_and_bytes_are_counted_per_source_and_day(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let public = public_app(state);
    let project = seed_project(&pool).await;
    let (a, token_a) = seed_source(&pool, project, "a").await;
    let (b, token_b) = seed_source(&pool, project, "b").await;

    post_json(
        &public,
        &token_a,
        json!([{ "message": "one" }, { "message": "two", "k": "v" }]),
    )
    .await;
    post_json(&public, &token_a, json!({ "message": "three" })).await;
    post_json(&public, &token_b, json!({ "message": "four" })).await;
    let pending = logs.usage.pending(a, Utc::now().date_naive());
    assert_eq!(pending.events, 3, "counted in memory first");
    assert_eq!(
        logs.usage.flush(&pool).await.unwrap(),
        2,
        "one row per source and day"
    );
    assert_eq!(
        logs.usage.pending(a, Utc::now().date_naive()),
        UsageCounters::default()
    );

    let counted = usage_row(&pool, a).await;
    assert_eq!(counted.events, 3);
    assert_eq!(
        counted.bytes,
        stored_bytes(&ch, a).await,
        "bytes = message + fields as stored"
    );
    assert_eq!(usage_row(&pool, b).await.events, 1);

    // Flushes add up (another instance's flush would too).
    post_json(&public, &token_a, json!({ "message": "five" })).await;
    logs.usage.flush(&pool).await.unwrap();
    assert_eq!(usage_row(&pool, a).await.events, 4);
    assert_eq!(usage_row(&pool, a).await.bytes, stored_bytes(&ch, a).await);
    assert_eq!(
        logs.usage.flush(&pool).await.unwrap(),
        0,
        "nothing pending, nothing written"
    );
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_usage_endpoint_returns_zero_filled_days_for_viewers(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let public = public_app(state.clone());
    let private = app(state);
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (a, token) = seed_source(&pool, project, "a").await;
    let (foreign, _) = seed_source(&pool, other, "theirs").await;
    let (_, _, viewer) = user_with_role(&private, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&private, &pool, project, None).await;
    post_json(
        &public,
        &token,
        json!([{ "message": "one" }, { "message": "two" }]),
    )
    .await;
    logs.usage.flush(&pool).await.unwrap();

    let base = format!("/projects/{project}/sources/{a}/usage");
    let (status, days) = call(
        &private,
        "GET",
        &format!("{base}?days=3"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{days}");
    let days = days.as_array().unwrap();
    assert_eq!(days.len(), 3);
    let today = Utc::now().date_naive();
    assert_eq!(days[2]["day"], today.to_string());
    assert_eq!(days[2]["events"], 2);
    assert_eq!(days[2]["bytes"], stored_bytes(&ch, a).await);
    assert_eq!(
        days[0]["day"],
        (today - chrono::Duration::days(2)).to_string()
    );
    assert_eq!(days[0]["events"], 0);
    for key in [
        "events_dropped",
        "events_sampled_out",
        "requests_rate_limited",
        "requests_over_quota",
    ] {
        assert_eq!(days[2][key], 0, "{key}");
    }

    let (_, default) = call(&private, "GET", &base, Some(&viewer), None).await;
    assert_eq!(default.as_array().unwrap().len(), 30);
    let (_, longest) = call(
        &private,
        "GET",
        &format!("{base}?days=400"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(longest.as_array().unwrap().len(), 400);
    for query in ["?days=0", "?days=401", "?days=-1", "?days=abc", "?days="] {
        let (status, json) = call(
            &private,
            "GET",
            &format!("{base}{query}"),
            Some(&viewer),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {json}");
        assert!(
            json["error"].as_str().unwrap().contains("days"),
            "a JSON error that names the parameter: {json}"
        );
    }
    assert_eq!(
        call(&private, "GET", &base, Some(&outsider), None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&private, "GET", &base, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    let foreign_usage = format!("/projects/{project}/sources/{foreign}/usage");
    assert_eq!(
        call(&private, "GET", &foreign_usage, Some(&viewer), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    ch.drop_database().await.unwrap();
}

/// Security: a source over its quota is refused with a distinct 429 while
/// the project's other sources keep ingesting.
#[sqlx::test]
async fn a_source_over_its_quota_gets_429_quota_exceeded_and_others_keep_ingesting(
    pool: sqlx::PgPool,
) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let public = public_app(state.clone());
    let project = seed_project(&pool).await;
    let quota = SourceConfig {
        daily_byte_quota: Some(1024 * 1024),
        ..source_config("capped")
    };
    let (capped, token) = seed_source_with(&pool, project, quota).await;
    let (_, other_token) = seed_source(&pool, project, "free").await;

    // 20 events x 64 KiB messages = 1.25 MiB: the request that crosses the
    // quota is accepted whole.
    let big = Value::Array(vec![json!({ "message": "x".repeat(64 * 1024) }); 20]);
    assert_eq!(
        post_json(&public, &token, big).await,
        (StatusCode::ACCEPTED, json!({ "accepted": 20 }))
    );

    let (status, headers, json) = post_logs(
        &public,
        Some(&token),
        &[("content-type", "application/json")],
        "{\"message\":\"m\"}",
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(json["code"], "quota_exceeded");
    let retry_after: u64 = headers["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((1..=86_400).contains(&retry_after), "{retry_after}");
    // The quota is checked before the body is looked at: a request that
    // would be a 400 or a 413 is a 429 first, and costs no parsing.
    let (status, _, json) = post_logs(
        &public,
        Some(&token),
        &[
            ("content-type", "application/json"),
            ("content-length", "20971520"),
        ],
        "{not json",
    )
    .await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{json}");
    assert_eq!(json["code"], "quota_exceeded");
    assert_eq!(
        post_json(&public, &other_token, json!({ "message": "m" }))
            .await
            .0,
        StatusCode::ACCEPTED
    );

    logs.usage.flush(&pool).await.unwrap();
    let counted = usage_row(&pool, capped).await;
    assert_eq!((counted.events, counted.requests_over_quota), (20, 2));
    assert!(counted.bytes >= 1024 * 1024);

    // Another instance (fresh in-memory state) reads the flushed bytes.
    let fresh = app_with_fresh_logs(&state);
    assert_eq!(
        post_json(&fresh, &token, json!({ "message": "m" })).await.0,
        StatusCode::TOO_MANY_REQUESTS
    );

    // Raising the quota takes effect on the next request.
    sqlx::query("update sources set daily_byte_quota = 104857600 where id = $1")
        .bind(capped)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        post_json(&public, &token, json!({ "message": "m" }))
            .await
            .0,
        StatusCode::ACCEPTED
    );
    assert_eq!(stored_rows(&ch, project).await.len(), 22);
    ch.drop_database().await.unwrap();
}

/// The public listener's router over a state that shares everything with
/// `state` but the ingest counters, as another engine instance would.
fn app_with_fresh_logs(state: &AppState) -> axum::Router {
    public_app(AppState {
        logs: Arc::new(LogsState::default()),
        ..state.clone()
    })
}

#[sqlx::test]
async fn counters_of_deleted_sources_are_dropped_on_flush(pool: sqlx::PgPool) {
    let tracker = usage::UsageTracker::default();
    tracker.record_accepted(Uuid::new_v4(), Utc::now().date_naive(), 5, 500);
    assert_eq!(tracker.flush(&pool).await.unwrap(), 0);
    let rows: i64 = sqlx::query_scalar("select count(*) from source_usage_daily")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
}

#[sqlx::test]
async fn a_failed_flush_keeps_every_counter_for_the_next_one(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let (source_id, _) = seed_source(&pool, project, "a").await;
    let today = Utc::now().date_naive();
    let tracker = usage::UsageTracker::default();
    tracker.record_accepted(source_id, today, 3, 300);
    tracker.record_rate_limited(source_id, today);

    assert!(tracker.flush(&unreachable_pool()).await.is_err());
    let kept = tracker.pending(source_id, today);
    assert_eq!(
        (kept.events, kept.bytes, kept.requests_rate_limited),
        (3, 300, 1)
    );

    assert_eq!(tracker.flush(&pool).await.unwrap(), 1);
    let counted = usage_row(&pool, source_id).await;
    assert_eq!(
        (counted.events, counted.bytes, counted.requests_rate_limited),
        (3, 300, 1)
    );
}

/// A flush takes its batch out of the counters first, so recording never
/// waits for the database. What is recorded while one runs, or while a
/// failing one waits to give up, must all arrive in the end.
#[sqlx::test]
async fn counters_recorded_during_a_flush_are_never_lost(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let (source_id, _) = seed_source(&pool, project, "a").await;
    let today = Utc::now().date_naive();
    let tracker = Arc::new(usage::UsageTracker::default());
    tracker.record_accepted(source_id, today, 1, 10);

    // The failing flush takes the first event out, then waits for a
    // connection that never comes.
    let failing = tokio::spawn({
        let tracker = tracker.clone();
        async move { tracker.flush(&unreachable_pool()).await.is_err() }
    });
    tokio::task::yield_now().await;
    assert_eq!(
        tracker.pending(source_id, today),
        UsageCounters::default(),
        "the batch is out of the counters while the flush runs"
    );
    for _ in 0..50 {
        tracker.record_accepted(source_id, today, 1, 10);
        tokio::task::yield_now().await;
    }
    assert!(failing.await.unwrap());
    let kept = tracker.pending(source_id, today);
    assert_eq!(
        (kept.events, kept.bytes),
        (51, 510),
        "the failed batch is added back to what arrived meanwhile"
    );

    // The same while a flush that works is on its way to the database.
    let flushing = tokio::spawn({
        let (tracker, pool) = (tracker.clone(), pool.clone());
        async move { tracker.flush(&pool).await.unwrap() }
    });
    tokio::task::yield_now().await;
    for _ in 0..50 {
        tracker.record_accepted(source_id, today, 1, 10);
        tokio::task::yield_now().await;
    }
    assert_eq!(flushing.await.unwrap(), 1);
    assert_eq!(tracker.flush(&pool).await.unwrap(), 1);
    let counted = usage_row(&pool, source_id).await;
    assert_eq!((counted.events, counted.bytes), (101, 1010));
}

/// The quota check serves the flushed total from memory (every flush
/// refreshes it, and it is at most 10 s old otherwise) plus this instance's
/// unflushed delta. It asks the database only when it holds no total for the
/// day.
#[sqlx::test]
async fn the_quota_check_uses_the_flushed_total_and_the_unflushed_delta(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let (source_id, _) = seed_source(&pool, project, "a").await;
    let today = Utc::now().date_naive();
    let tracker = usage::UsageTracker::default();
    let offline = unreachable_pool();

    assert_eq!(
        tracker.bytes_today(&pool, source_id, today).await.unwrap(),
        0,
        "nothing known yet: the database says nothing was counted"
    );
    // Another instance flushes 5000 bytes. This one does not see them yet:
    // its total is cached, and the database is not asked again.
    sqlx::query("insert into source_usage_daily (source_id, day, bytes) values ($1, $2, 5000)")
        .bind(source_id)
        .bind(today)
        .execute(&pool)
        .await
        .unwrap();
    tracker.record_accepted(source_id, today, 1, 300);
    assert_eq!(
        tracker
            .bytes_today(&offline, source_id, today)
            .await
            .unwrap(),
        300,
        "the cached total plus what this instance has not flushed"
    );

    // A flush returns the total over every instance and refreshes the cache.
    tracker.flush(&pool).await.unwrap();
    tracker.record_accepted(source_id, today, 1, 20);
    assert_eq!(
        tracker
            .bytes_today(&offline, source_id, today)
            .await
            .unwrap(),
        5320
    );

    // A total is for one day: tomorrow is asked of the database.
    let tomorrow = today + chrono::Duration::days(1);
    assert!(
        tracker
            .bytes_today(&offline, source_id, tomorrow)
            .await
            .is_err()
    );
}

/// Engine instances flush the same sources at the same moment. A flush is one
/// statement that locks the rows it upserts as it goes: if each wrote them in
/// an order of its own (a HashMap's, different for every tracker), two of them
/// could each wait for a row the other holds and Postgres would kill one with
/// a deadlock. So every flush writes in key order. Rows of a statement land in
/// the heap in the order it produced them, which `ctid` shows.
#[sqlx::test]
async fn a_flush_writes_its_rows_in_key_order(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let mut sources = Vec::new();
    for n in 0..60 {
        sources.push(seed_source(&pool, project, &format!("s{n}")).await.0);
    }
    let today = Utc::now().date_naive();
    let tracker = usage::UsageTracker::default();
    for (n, source) in sources.iter().enumerate() {
        tracker.record_accepted(*source, today, 1, n as u64);
        tracker.record_accepted(*source, today - chrono::Duration::days(1), 1, n as u64);
    }
    assert_eq!(tracker.flush(&pool).await.unwrap(), 120);

    let written: Vec<(Uuid, chrono::NaiveDate)> =
        sqlx::query_as("select source_id, day from source_usage_daily order by ctid")
            .fetch_all(&pool)
            .await
            .unwrap();
    let mut by_key = written.clone();
    by_key.sort();
    assert_eq!(written.len(), 120);
    assert_eq!(written, by_key, "rows are written by source, then day");
}

#[sqlx::test]
async fn usage_older_than_400_days_is_pruned(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    let (source_id, _) = seed_source(&pool, project, "a").await;
    let today = Utc::now().date_naive();
    for age in [401, 400, 399] {
        sqlx::query("insert into source_usage_daily (source_id, day, events) values ($1, $2, 1)")
            .bind(source_id)
            .bind(today - chrono::Duration::days(age))
            .execute(&pool)
            .await
            .unwrap();
    }
    assert_eq!(usage::prune(&pool, today).await.unwrap(), 2);
    let oldest: chrono::NaiveDate = sqlx::query_scalar("select min(day) from source_usage_daily")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        oldest,
        today - chrono::Duration::days(399),
        "400 days are kept, today included"
    );
}
