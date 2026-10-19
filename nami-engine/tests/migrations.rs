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
