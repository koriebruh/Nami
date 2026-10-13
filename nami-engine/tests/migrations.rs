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
        "email_verification_tokens",
        "password_reset_tokens",
        "monitors",
        "incidents",
        "heartbeats",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "missing table {expected}"
        );
    }
}
