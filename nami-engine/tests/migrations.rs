#[sqlx::test]
async fn all_tables_exist(pool: sqlx::PgPool) {
    let rows: Vec<(String,)> = sqlx::query_as(
        "select table_name from information_schema.tables where table_schema = 'public'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    let names: Vec<String> = rows.into_iter().map(|(n,)| n).collect();
    for expected in [
        "users",
        "oauth_identities",
        "projects",
        "project_members",
        "sources",
        "source_usage_daily",
        "email_verification_tokens",
        "password_reset_tokens",
        "monitors",
        "incidents",
        "heartbeats",
        "worker_state",
        "maintenance_windows",
        "monitor_groups",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "missing table {expected}"
        );
    }
}

/// Every incident of one monitor or heartbeat is reachable through an index
/// (per-source incident lists, and the cascades that delete a monitor's or a
/// heartbeat's incidents), not only the open ones.
#[sqlx::test]
async fn incidents_are_indexed_by_their_source(pool: sqlx::PgPool) {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "select indexname, indexdef from pg_indexes
         where schemaname = 'public' and tablename = 'incidents'
           and indexname in ('incidents_monitor_idx', 'incidents_heartbeat_idx')
         order by indexname",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    assert_eq!(rows.len(), 2, "{rows:?}");
    let (heartbeat, monitor) = (&rows[0].1, &rows[1].1);
    assert!(
        heartbeat.contains("(heartbeat_id) WHERE (heartbeat_id IS NOT NULL)"),
        "{heartbeat}"
    );
    assert!(
        monitor.contains("(monitor_id) WHERE (monitor_id IS NOT NULL)"),
        "{monitor}"
    );
}

/// Incidents carry the output of the heartbeat ping that opened them, capped
/// in the database too.
#[sqlx::test]
async fn incidents_have_a_bounded_details_column(pool: sqlx::PgPool) {
    let columns: Vec<(String,)> = sqlx::query_as(
        "select column_name from information_schema.columns
         where table_schema = 'public' and table_name = 'incidents' and column_name = 'details'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(columns.len(), 1);
    let project = common_project(&pool).await;
    let too_long = sqlx::query(
        "insert into incidents (project_id, title, details) values ($1, 't', repeat('a', 10241 + 16))",
    )
    .bind(project)
    .execute(&pool)
    .await;
    assert!(too_long.is_err(), "details beyond the cap must be refused");
}

async fn common_project(pool: &sqlx::PgPool) -> uuid::Uuid {
    sqlx::query_scalar("insert into projects (name, slug) values ('p', 'p') returning id")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A project and one source in it, inserted with plain SQL (no engine code).
async fn seed_source_row(pool: &sqlx::PgPool) -> (uuid::Uuid, uuid::Uuid) {
    let project: uuid::Uuid = sqlx::query_scalar(
        "insert into projects (name, slug) values ('p', gen_random_uuid()::text) returning id",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let source: uuid::Uuid = sqlx::query_scalar(
        "insert into sources (project_id, name, token_hash, token_enc)
         values ($1, 'api', md5(gen_random_uuid()::text), '\\x00'::bytea) returning id",
    )
    .bind(project)
    .fetch_one(pool)
    .await
    .unwrap();
    (project, source)
}

#[sqlx::test]
async fn sources_have_an_empty_pipeline_by_default_and_reject_non_arrays(pool: sqlx::PgPool) {
    let (_, source) = seed_source_row(&pool).await;
    let pipeline: serde_json::Value =
        sqlx::query_scalar("select pipeline from sources where id = $1")
            .bind(source)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pipeline, serde_json::json!([]));
    for bad in [r#"{}"#, r#""x""#] {
        let result = sqlx::query("update sources set pipeline = $1::jsonb where id = $2")
            .bind(bad)
            .bind(source)
            .execute(&pool)
            .await;
        assert!(result.is_err(), "{bad} must be refused");
    }
    let too_many = serde_json::Value::Array(vec![serde_json::json!({"type":"drop"}); 51]);
    assert!(
        sqlx::query("update sources set pipeline = $1 where id = $2")
            .bind(too_many)
            .bind(source)
            .execute(&pool)
            .await
            .is_err()
    );
}
