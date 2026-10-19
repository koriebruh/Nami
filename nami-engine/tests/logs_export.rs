// Streamed export (`GET /projects/{id}/logs/export`).
// Requires `docker compose up -d postgres clickhouse`.
mod common;

use std::{sync::Arc, time::Duration};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use chrono::{SecondsFormat, Utc};
use common::{
    app, call,
    logs::{post_json, seed_source},
    public_app, seed_project, state_with_clickhouse, user_with_role,
};
use nami_engine::{
    clickhouse::ClickHouse,
    logs::{
        LogsState,
        export::{ExportLimits, MAX_EXPORTS_PER_INSTANCE},
    },
    state::AppState,
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

fn encode(text: &str) -> String {
    form_urlencoded::byte_serialize(text.as_bytes()).collect()
}

fn window() -> String {
    let to = Utc::now() + chrono::Duration::minutes(1);
    let from = to - chrono::Duration::hours(1);
    format!(
        "from={}&to={}",
        encode(&from.to_rfc3339_opts(SecondsFormat::Millis, true)),
        encode(&to.to_rfc3339_opts(SecondsFormat::Millis, true))
    )
}

async fn plant(ch: &ClickHouse, project: Uuid, source: Uuid, count: u64) {
    // Test-only SQL with the ids formatted in: the ids are UUIDs this test made.
    ch.execute(&format!(
        "INSERT INTO logs (id, project_id, source_id, dt, level, message, fields, trace_id, span_id, pattern, retention_days) \
         SELECT generateUUIDv4(), toUUID('{project}'), toUUID('{source}'), now64(3) - toIntervalMillisecond(number), \
         'info', concat('row ', toString(number)), '{{}}', '', '', 'row <num>', 30 FROM numbers({count})"
    ))
    .await
    .unwrap();
}

async fn get(
    app: &axum::Router,
    uri: &str,
    cookie: &axum::http::HeaderValue,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::get(uri)
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn text(response: axum::response::Response) -> String {
    String::from_utf8(
        axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap()
            .to_vec(),
    )
    .unwrap()
}

#[sqlx::test]
async fn ndjson_carries_every_column_and_csv_the_picked_ones(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, foreign) = seed_source(&pool, other, "theirs").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    post_json(
        &public,
        &token,
        json!([
            { "message": "=cmd|' /C calc'!A0", "user": { "name": "@SUM(1)" }, "n": 5 },
            { "message": "plain, \"quoted\"", "user": { "name": "+1" }, "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736" },
            { "message": "-2" }
        ]),
    )
    .await;
    post_json(&public, &foreign, json!({ "message": "never exported" })).await;

    let response = get(
        &app,
        &format!("/projects/{project}/logs/export?format=ndjson&{}", window()),
        &viewer,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers().clone();
    assert_eq!(headers["content-type"], "application/x-ndjson");
    assert_eq!(headers["x-nami-export-limit"], "100000");
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    let disposition = headers["content-disposition"].to_str().unwrap();
    assert!(
        regex::Regex::new(r#"^attachment; filename="nami-logs-\d{8}T\d{6}Z\.ndjson"$"#)
            .unwrap()
            .is_match(disposition),
        "{disposition}"
    );
    let body = text(response).await;
    let lines: Vec<Value> = body
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 3, "never another project's rows: {body}");
    let mut keys: Vec<&str> = lines[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "dt",
            "fields",
            "id",
            "level",
            "message",
            "pattern",
            "pattern_hash",
            "project_id",
            "retention_days",
            "source_id",
            "span_id",
            "trace_id"
        ]
    );
    assert!(lines.iter().all(|l| l["project_id"] == project.to_string()));

    let response = get(
        &app,
        &format!(
            "/projects/{project}/logs/export?format=csv&columns=user.name&columns=n&{}",
            window()
        ),
        &viewer,
    )
    .await;
    assert_eq!(
        response.headers()["content-type"],
        "text/csv; charset=utf-8"
    );
    let csv = text(response).await;
    let mut records = csv.split("\r\n");
    assert_eq!(
        records.next().unwrap(),
        "\"dt\",\"level\",\"source\",\"message\",\"trace_id\",\"user.name\",\"n\""
    );
    assert!(
        csv.contains(",\"api\",\"'=cmd|' /C calc'!A0\",\"\",\"'@SUM(1)\",\"5\"\r\n"),
        "{csv}"
    );
    assert!(
        csv.contains(",\"api\",\"plain, \"\"quoted\"\"\",\"4bf92f3577b34da6a3ce929d0e0e4736\",\"'+1\",\"\"\r\n"),
        "{csv}"
    );
    assert!(csv.contains(",\"api\",\"'-2\",\"\",\"\",\"\"\r\n"), "{csv}");
    assert!(!csv.contains("never exported"));
    ch.drop_database().await.unwrap();
}

/// Hostile text survives NDJSON intact (one JSON object per line) and
/// comes out of CSV as one record per row.
#[sqlx::test]
async fn hostile_text_keeps_both_formats_well_formed(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let (_, token) = seed_source(&pool, project, "api").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let hostile = "a\nb\r\n\"q\",{\"id\":1}\\ \u{2028} 𝄞 \t=1";
    post_json(&public, &token, json!({ "message": hostile, "k": hostile })).await;

    let body = text(
        get(
            &app,
            &format!("/projects/{project}/logs/export?format=ndjson&{}", window()),
            &viewer,
        )
        .await,
    )
    .await;
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 1, "{body}");
    let row: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(row["message"], hostile);

    let csv = text(
        get(
            &app,
            &format!(
                "/projects/{project}/logs/export?format=csv&columns=k&{}",
                window()
            ),
            &viewer,
        )
        .await,
    )
    .await;
    let mut reader = csv::ReaderBuilder::new().from_reader(csv.as_bytes());
    let records: Vec<csv::StringRecord> = reader.records().map(Result::unwrap).collect();
    assert_eq!(records.len(), 1, "{csv}");
    assert_eq!(&records[0][3], hostile);
    assert_eq!(&records[0][5], hostile);
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn the_row_cap_holds_and_is_announced(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let app = app(state);
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    plant(&ch, project, source, 100_005).await;
    let response = get(
        &app,
        &format!("/projects/{project}/logs/export?format=ndjson&{}", window()),
        &viewer,
    )
    .await;
    assert_eq!(response.headers()["x-nami-export-limit"], "100000");
    let body = text(response).await;
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 100_000);
    assert!(lines[0].contains("\"row 0\""), "newest first: {}", lines[0]);
    assert!(
        !body.contains("_nami_export_error"),
        "the row cap is a normal end"
    );
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn hitting_the_byte_cap_ends_with_an_error_trailer(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = LogsState {
        export_limits: ExportLimits {
            bytes: 4096,
            ..ExportLimits::default()
        },
        ..LogsState::default()
    };
    let state = AppState {
        logs: Arc::new(logs),
        ..state
    };
    let app = app(state);
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    plant(&ch, project, source, 500).await;
    for format in ["ndjson", "csv"] {
        let body = text(
            get(
                &app,
                &format!(
                    "/projects/{project}/logs/export?format={format}&{}",
                    window()
                ),
                &viewer,
            )
            .await,
        )
        .await;
        let last = body.trim_end().lines().last().unwrap();
        assert!(body.len() < 4096 + 256, "{format}: {}", body.len());
        match format {
            "ndjson" => assert!(last.starts_with("{\"_nami_export_error\":"), "{last}"),
            _ => assert!(last.starts_with("\"# export stopped early:"), "{last}"),
        }
    }
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_third_concurrent_export_is_refused(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let app = app(state);
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (user, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    plant(&ch, project, source, 100_000).await;
    let uri = format!("/projects/{project}/logs/export?format=ndjson&{}", window());
    // Two streams held open, their bodies unread (each is far larger than the channel).
    let first = get(&app, &uri, &viewer).await;
    let second = get(&app, &uri, &viewer).await;
    assert_eq!(
        (first.status(), second.status()),
        (StatusCode::OK, StatusCode::OK)
    );
    let third = get(&app, &uri, &viewer).await;
    assert_eq!(third.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(third.headers()["retry-after"], "5");
    assert_eq!(third.headers()["cache-control"], "no-store");
    assert_eq!(
        serde_json::from_str::<Value>(&text(third).await).unwrap(),
        json!({ "error": "you already have 2 exports running; retry when one finishes" })
    );
    // The client going away gives its slot back.
    drop(first);
    common::logs::eventually("an export slot comes back", Duration::from_secs(5), || {
        logs.export_slots.open(user) < 2
    })
    .await;
    assert_eq!(get(&app, &uri, &viewer).await.status(), StatusCode::OK);
    drop(second);
    common::logs::eventually("every slot comes back", Duration::from_secs(5), || {
        logs.export_slots.total() == 0
    })
    .await;
    ch.drop_database().await.unwrap();
}

/// The instance-wide cap: with every slot taken by other users, a user
/// with none of their own is refused too, with a 503 (the user's own cap is
/// the 429).
#[sqlx::test]
async fn the_instance_cap_holds_across_users(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let app = app(state);
    let project = seed_project(&pool).await;
    let (user, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let uri = format!("/projects/{project}/logs/export?format=ndjson&{}", window());
    let held: Vec<_> = (0..MAX_EXPORTS_PER_INSTANCE)
        .map(|_| {
            logs.export_slots
                .acquire_capped(Uuid::new_v4(), 2, MAX_EXPORTS_PER_INSTANCE)
                .unwrap()
        })
        .collect();
    let busy = get(&app, &uri, &viewer).await;
    assert_eq!(busy.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(busy.headers()["retry-after"], "5");
    assert_eq!(
        serde_json::from_str::<Value>(&text(busy).await).unwrap(),
        json!({ "error": "busy, retry shortly" })
    );
    assert_eq!(logs.export_slots.open(user), 0, "a refusal holds nothing");
    drop(held);
    let response = get(&app, &uri, &viewer).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(text(response).await, "");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn a_stalled_export_gives_its_slot_back_at_the_deadline(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let limits = ExportLimits {
        deadline: Duration::from_secs(1),
        ..ExportLimits::default()
    };
    let logs = Arc::new(LogsState {
        export_limits: limits,
        ..LogsState::default()
    });
    let state = AppState {
        logs: logs.clone(),
        ..state
    };
    let app = app(state);
    let project = seed_project(&pool).await;
    let (source, _) = seed_source(&pool, project, "api").await;
    let (user, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    plant(&ch, project, source, 100_000).await;
    let uri = format!("/projects/{project}/logs/export?format=ndjson&{}", window());
    // Connected but never read: the channel fills and the task waits on its send.
    let stalled = get(&app, &uri, &viewer).await;
    assert_eq!(stalled.status(), StatusCode::OK);
    assert_eq!(logs.export_slots.open(user), 1);
    common::logs::eventually(
        "the deadline frees the slot",
        Duration::from_secs(5),
        || logs.export_slots.open(user) == 0,
    )
    .await;
    // Reading now: what was queued, then a cut, never a clean end.
    let read = axum::body::to_bytes(stalled.into_body(), usize::MAX).await;
    assert!(read.is_err(), "a stalled export must not end cleanly");
    ch.drop_database().await.unwrap();
}

#[sqlx::test]
async fn export_needs_access_and_valid_parameters(pool: sqlx::PgPool) {
    let (state, ch) = state_with_clickhouse(&pool).await;
    let logs = state.logs.clone();
    let public = public_app(state.clone());
    let app = app(state);
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (own_source, token) = seed_source(&pool, project, "api").await;
    let (foreign_source, foreign_token) = seed_source(&pool, other, "theirs").await;
    let (user, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    // Rows the regex runs on (ClickHouse only compiles it when a row with
    // the field reaches it).
    plant(&ch, project, own_source, 9).await;
    post_json(&public, &token, json!({ "message": "m", "a": "500" })).await;
    post_json(&public, &foreign_token, json!({ "message": "theirs" })).await;
    let base = format!("/projects/{project}/logs/export");
    assert_eq!(
        call(&app, "GET", &format!("{base}?format=csv"), None, None)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        call(
            &app,
            "GET",
            &format!("{base}?format=csv"),
            Some(&outsider),
            None
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let regex = encode(&json!({ "path": "a", "op": "regex", "value": "(?x)5 0 0" }).to_string());
    for bad in [
        "format=xlsx".to_string(),
        String::new(),
        "format=csv&columns=a..b".to_string(),
        "format=csv&filter=%7B%22path%22%3A%22a%22%2C%22op%22%3A%22gt%22%2C%22value%22%3A%22x%22%7D".to_string(),
        // Valid for Rust's regex crate, refused by ClickHouse's RE2: a 400
        // before the file starts, not a broken file.
        format!("format=ndjson&filter={regex}&{}", window()),
    ] {
        let (status, json) = call(&app, "GET", &format!("{base}?{bad}"), Some(&viewer), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {json}");
        assert!(json["error"].is_string(), "{json}");
    }
    // A source of another project (or of none) matches nothing, as in
    // search: an NDJSON file with no line, a CSV file with only its header.
    for source in [foreign_source, Uuid::new_v4()] {
        let response = get(
            &app,
            &format!("{base}?format=csv&source_id={source}&{}", window()),
            &viewer,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            text(response).await,
            "\"dt\",\"level\",\"source\",\"message\",\"trace_id\"\r\n"
        );
        let response = get(
            &app,
            &format!("{base}?format=ndjson&source_id={source}&{}", window()),
            &viewer,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            text(response).await,
            "",
            "another project's source exports nothing"
        );
    }
    let response = get(
        &app,
        &format!("{base}?format=ndjson&source_id={own_source}&{}", window()),
        &viewer,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(text(response).await.lines().count(), 10);
    // No refused request held a slot.
    assert_eq!(logs.export_slots.open(user), 0);
    ch.drop_database().await.unwrap();
}
