use crate::common;

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
        "log_views",
        "log_alerts",
        "incident_severities",
        "incident_events",
        "notification_channels",
        "on_call_schedules",
        "on_call_participants",
        "on_call_overrides",
        "notification_deliveries",
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

/// Advanced request options (NAMI-UP-23): IP version, cookie jar and proxy,
/// with credentials only beside a proxy url, enforced in the database too.
#[sqlx::test]
async fn monitors_have_the_advanced_request_columns(pool: sqlx::PgPool) {
    let rows: Vec<(String,)> = sqlx::query_as(
        "select column_name from information_schema.columns
         where table_schema = 'public' and table_name = 'monitors'
           and column_name in ('ip_version', 'remember_cookies', 'proxy_url',
                               'proxy_username', 'proxy_password_enc')",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 5, "{rows:?}");
    let project = common_project(&pool).await;
    let insert = |extra: &'static str| {
        let pool = pool.clone();
        async move {
            let sql = match extra {
                "ip" => {
                    "insert into monitors (project_id, name, check_type, ip_version) values ($1, 'm', 'http', 'v4')"
                }
                "user" => {
                    "insert into monitors (project_id, name, check_type, proxy_username) values ($1, 'm', 'http', 'u')"
                }
                "password" => {
                    "insert into monitors (project_id, name, check_type, proxy_password_enc) values ($1, 'm', 'http', '\\x01')"
                }
                _ => {
                    "insert into monitors (project_id, name, check_type, proxy_url, proxy_username, proxy_password_enc) values ($1, 'm', 'http', 'http://p:1', 'u', '\\x01')"
                }
            };
            sqlx::query(sql).bind(project).execute(&pool).await
        }
    };
    assert!(insert("ip").await.is_err(), "unknown ip_version");
    assert!(
        insert("user").await.is_err(),
        "proxy_username without proxy_url"
    );
    assert!(
        insert("password").await.is_err(),
        "proxy password without proxy_url"
    );
    insert("ok").await.unwrap();
    let (ip_version, cookies): (String, bool) =
        sqlx::query_as("select ip_version, remember_cookies from monitors where project_id = $1")
            .bind(project)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((ip_version.as_str(), cookies), ("any", false));
}

#[sqlx::test]
async fn log_views_names_are_unique_per_project(pool: sqlx::PgPool) {
    let (project, _) = seed_source_row(&pool).await;
    let (other, _) = seed_source_row(&pool).await;
    let insert = |project: uuid::Uuid| {
        sqlx::query("insert into log_views (project_id, name) values ($1, 'Errors')")
            .bind(project)
            .execute(&pool)
    };
    insert(project).await.unwrap();
    let error = insert(project).await.unwrap_err();
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23505")
    );
    insert(other).await.unwrap();
    let bad = sqlx::query("insert into log_views (project_id, name, query) values ($1, 'x', '[]')")
        .bind(project)
        .execute(&pool)
        .await;
    assert!(bad.is_err(), "a non-object query must be refused");
}

#[sqlx::test]
async fn log_alerts_allow_one_open_incident_each(pool: sqlx::PgPool) {
    let (project, _) = seed_source_row(&pool).await;
    let alert: uuid::Uuid = sqlx::query_scalar(
        "insert into log_alerts (project_id, name, threshold, comparison, window_seconds, interval_seconds)
         values ($1, 'Errors', 10, 'gt', 300, 60) returning id",
    )
    .bind(project)
    .fetch_one(&pool)
    .await
    .unwrap();
    let open = || {
        sqlx::query("insert into incidents (project_id, log_alert_id, title) values ($1, $2, 't')")
            .bind(project)
            .bind(alert)
            .execute(&pool)
    };
    open().await.unwrap();
    let error = open().await.unwrap_err();
    assert_eq!(
        error.as_database_error().and_then(|e| e.code()).as_deref(),
        Some("23505")
    );
    let never_fires = sqlx::query(
        "insert into log_alerts (project_id, name, threshold, comparison, window_seconds, interval_seconds)
         values ($1, 'Quiet', 0, 'lt', 300, 60)",
    )
    .bind(project)
    .execute(&pool)
    .await;
    assert!(never_fires.is_err(), "lt 0 can never fire and must be refused");
}

/// Every constraint of `log_alerts` carries the name its migration gives it
/// (STANDARDS §5), primary and foreign keys included.
#[sqlx::test]
async fn log_alerts_constraints_are_all_named(pool: sqlx::PgPool) {
    let names: Vec<String> = sqlx::query_scalar(
        "select conname::text from pg_constraint
         where conrelid = 'public.log_alerts'::regclass order by conname",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        names,
        [
            "log_alerts_comparison_known",
            "log_alerts_created_by_fkey",
            "log_alerts_interval_range",
            "log_alerts_lt_needs_a_threshold",
            "log_alerts_pkey",
            "log_alerts_project_id_fkey",
            "log_alerts_query_is_object",
            "log_alerts_state_known",
            "log_alerts_threshold_range",
            "log_alerts_window_range",
        ]
    );
}

#[sqlx::test]
async fn every_project_gets_the_three_default_severities(pool: sqlx::PgPool) {
    let project = common::seed_project(&pool).await;
    let rows: Vec<(String, i32, bool)> = sqlx::query_as(
        "select name, rank, is_default from incident_severities where project_id = $1 order by rank",
    )
    .bind(project)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        [
            ("critical".into(), 1, false),
            ("high".into(), 2, true),
            ("low".into(), 3, false)
        ]
    );
    let second_default = sqlx::query(
        "insert into incident_severities (project_id, name, rank, is_default) values ($1, 'x', 9, true)",
    )
    .bind(project)
    .execute(&pool)
    .await
    .unwrap_err();
    assert_eq!(
        second_default.as_database_error().unwrap().code().as_deref(),
        Some("23505")
    );
}

#[sqlx::test]
async fn an_acknowledged_incident_still_blocks_a_second_one_for_its_monitor(pool: sqlx::PgPool) {
    let project = common::seed_project(&pool).await;
    let monitor = common::seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9", 1).await;
    sqlx::query(
        "insert into incidents (project_id, monitor_id, title, status, acknowledged_at, source)
         values ($1, $2, 'down', 'acknowledged', now(), 'monitor')",
    )
    .bind(project)
    .bind(monitor)
    .execute(&pool)
    .await
    .unwrap();
    let second = sqlx::query(
        "insert into incidents (project_id, monitor_id, title, source) values ($1, $2, 'down', 'monitor')",
    )
    .bind(project)
    .bind(monitor)
    .execute(&pool)
    .await
    .unwrap_err();
    assert_eq!(
        second.as_database_error().unwrap().code().as_deref(),
        Some("23505")
    );
}

#[sqlx::test]
async fn a_plain_insert_takes_its_source_from_its_foreign_key(pool: sqlx::PgPool) {
    let project = common::seed_project(&pool).await;
    let monitor = common::seed_http_monitor(&pool, project, "api", "http://127.0.0.1:9", 1).await;
    let source: String = sqlx::query_scalar(
        "insert into incidents (project_id, monitor_id, title) values ($1, $2, 'down') returning source",
    )
    .bind(project)
    .bind(monitor)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(source, "monitor");
}

/// Every constraint of the incident lifecycle tables carries the name its
/// migration gives it (STANDARDS §5), primary and foreign keys included.
#[sqlx::test]
async fn incident_lifecycle_constraints_are_all_named(pool: sqlx::PgPool) {
    let names: Vec<String> = sqlx::query_scalar(
        "select conname::text from pg_constraint
         where conrelid in ('public.incident_severities'::regclass, 'public.incident_events'::regclass)
            or (conrelid = 'public.incidents'::regclass
                and conname not in ('incidents_pkey', 'incidents_project_id_fkey',
                                    'incidents_monitor_id_fkey', 'incidents_heartbeat_id_fkey',
                                    'incidents_details_check', 'incidents_log_alert_id_fkey'))
         order by conname",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        names,
        [
            "incident_events_actor_id_fkey",
            "incident_events_incident_id_fkey",
            "incident_events_kind_known",
            "incident_events_message_size",
            "incident_events_metadata_is_object",
            "incident_events_pkey",
            "incident_severities_name_format",
            "incident_severities_name_unique",
            "incident_severities_pkey",
            "incident_severities_project_id_fkey",
            "incident_severities_rank_range",
            "incident_severities_rank_unique",
            "incidents_acknowledged_by_fkey",
            "incidents_acknowledged_has_time",
            "incidents_created_by_fkey",
            "incidents_metadata_is_object",
            "incidents_postmortem_size",
            "incidents_resolved_by_fkey",
            "incidents_resolved_has_time",
            "incidents_severity_id_fkey",
            "incidents_source_known",
            "incidents_status_check",
        ]
    );
}

#[sqlx::test]
async fn a_channel_belongs_to_exactly_one_owner(pool: sqlx::PgPool) {
    let project = common::seed_project(&pool).await;
    let user = common::seed_user(&pool, &common::unique_email("owner"), "member").await;
    for (project_id, user_id, kind) in [
        (None, None, "slack"),
        (Some(project), Some(user), "slack"),
        (None, Some(user), "email"),
    ] {
        let error = sqlx::query(
            "insert into notification_channels (id, project_id, user_id, name, kind, config_enc, summary)
             values (gen_random_uuid(), $1, $2, 'x', $3, '\\x00', 'x')",
        )
        .bind(project_id)
        .bind(user_id)
        .bind(kind)
        .execute(&pool)
        .await
        .unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("23514"),
            "{project_id:?} {user_id:?} {kind}"
        );
    }
}

/// Every constraint of `notification_channels` carries the name its
/// migration gives it (STANDARDS §5), primary and foreign keys included.
#[sqlx::test]
async fn notification_channels_constraints_are_all_named(pool: sqlx::PgPool) {
    let names: Vec<String> = sqlx::query_scalar(
        "select conname::text from pg_constraint
         where conrelid = 'public.notification_channels'::regclass order by conname",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        names,
        [
            "notification_channels_created_by_fkey",
            "notification_channels_kind_known",
            "notification_channels_name_length",
            "notification_channels_no_personal_email",
            "notification_channels_one_owner",
            "notification_channels_pkey",
            "notification_channels_project_id_fkey",
            "notification_channels_user_id_fkey",
            "notification_channels_webhook_has_secret",
        ]
    );
}

#[sqlx::test]
async fn a_custom_rotation_needs_hours_and_others_must_not_have_them(pool: sqlx::PgPool) {
    let project = common::seed_project(&pool).await;
    for (rotation, hours) in [("custom", None), ("weekly", Some(24))] {
        let error = sqlx::query(
            "insert into on_call_schedules (project_id, name, timezone, rotation, rotation_hours, handoff_time, start_date)
             values ($1, $2, 'UTC', $2, $3, '09:00', '2026-10-19')",
        )
        .bind(project)
        .bind(rotation)
        .bind(hours)
        .execute(&pool)
        .await
        .unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().as_deref(),
            Some("23514"),
            "{rotation} {hours:?}"
        );
    }
}

/// Every constraint of the on-call tables carries the name its migration
/// gives it (STANDARDS §5), primary and foreign keys included.
#[sqlx::test]
async fn on_call_constraints_are_all_named(pool: sqlx::PgPool) {
    let names: Vec<String> = sqlx::query_scalar(
        "select conname::text from pg_constraint
         where conrelid in ('public.on_call_schedules'::regclass, 'public.on_call_participants'::regclass,
                            'public.on_call_overrides'::regclass)
         order by conname",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        names,
        [
            "on_call_overrides_created_by_fkey",
            "on_call_overrides_ends_after_start",
            "on_call_overrides_pkey",
            "on_call_overrides_schedule_id_fkey",
            "on_call_overrides_user_id_fkey",
            "on_call_participants_pkey",
            "on_call_participants_position_range",
            "on_call_participants_schedule_id_fkey",
            "on_call_participants_user_id_fkey",
            "on_call_participants_user_unique",
            "on_call_schedules_custom_has_hours",
            "on_call_schedules_name_unique",
            "on_call_schedules_pkey",
            "on_call_schedules_project_id_fkey",
            "on_call_schedules_restrictions_is_array",
            "on_call_schedules_rotation_hours_range",
            "on_call_schedules_rotation_known",
        ]
    );
}

#[sqlx::test]
async fn a_target_gets_one_delivery_per_incident_round(pool: sqlx::PgPool) {
    let project = common::seed_project(&pool).await;
    let user = common::seed_user(&pool, &common::unique_email("member"), "member").await;
    let incident = common::open_test_incident(&pool, project, user).await;
    let insert = || {
        sqlx::query(
            "insert into notification_deliveries (incident_id, event, round, target_key, user_id)
             values ($1, 'triggered', 'p0r0s0', 'email:x', $2)",
        )
        .bind(incident)
        .bind(user)
        .execute(&pool)
    };
    insert().await.unwrap();
    let error = insert().await.unwrap_err();
    assert_eq!(error.as_database_error().unwrap().code().as_deref(), Some("23505"));
}

/// Every constraint of `notification_deliveries` carries the name its
/// migration gives it (STANDARDS §5), primary and foreign keys included.
#[sqlx::test]
async fn notification_deliveries_constraints_are_all_named(pool: sqlx::PgPool) {
    let names: Vec<String> = sqlx::query_scalar(
        "select conname::text from pg_constraint
         where conrelid = 'public.notification_deliveries'::regclass order by conname",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        names,
        [
            "notification_deliveries_attempts_range",
            "notification_deliveries_channel_id_fkey",
            "notification_deliveries_event_known",
            "notification_deliveries_has_target",
            "notification_deliveries_incident_id_fkey",
            "notification_deliveries_last_error_length",
            "notification_deliveries_pkey",
            "notification_deliveries_round_length",
            "notification_deliveries_sent_has_time",
            "notification_deliveries_status_known",
            "notification_deliveries_target_unique",
            "notification_deliveries_user_id_fkey",
        ]
    );
}
