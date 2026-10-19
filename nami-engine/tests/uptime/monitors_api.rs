use crate::common;

use axum::{
    Router,
    http::{HeaderValue, StatusCode},
};
use common::{
    call, login, seed_http_monitor, seed_project, seed_user, test_app, unique_email, user_with_role,
};
use serde_json::{Value, json};
use tokio::task::JoinSet;
use uuid::Uuid;

fn http_monitor(name: &str) -> Value {
    json!({ "name": name, "check_type": "http", "url": "https://example.com/health" })
}

#[sqlx::test]
async fn member_creates_reads_and_updates_admin_deletes(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let collection = format!("/projects/{project}/monitors");

    let (status, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(http_monitor("API")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["project_id"], project.to_string());
    assert_eq!(created["http_method"], "GET", "defaults are applied");
    assert_eq!(created["expected_status_min"], 200);
    assert_eq!(created["expected_status_max"], 399);
    assert_eq!(created["interval_seconds"], 60);
    assert_eq!(created["timeout_seconds"], 10);
    assert_eq!(created["confirmation_threshold"], 2);
    assert_eq!(created["current_status"], "pending");
    let id = created["id"].as_str().unwrap().to_string();
    let item = format!("{collection}/{id}");

    let (status, list) = call(&app, "GET", &collection, Some(&member), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["id"], id.as_str());

    let (status, patched) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "name": "API v2", "paused": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["name"], "API v2");
    assert_eq!(patched["paused"], true);
    assert_eq!(
        patched["url"], "https://example.com/health",
        "fields absent from a PATCH are kept"
    );

    let (status, _) = call(&app, "DELETE", &item, Some(&member), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "members cannot delete");

    let (status, body) = call(&app, "DELETE", &item, Some(&admin), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({ "deleted": true }));
    let (status, _) = call(&app, "GET", &item, Some(&admin), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn viewer_can_read_but_not_write(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let collection = format!("/projects/{project}/monitors");
    let item = format!("{collection}/{monitor}");

    assert_eq!(
        call(&app, "GET", &collection, Some(&viewer), None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", &item, Some(&viewer), None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &collection,
            Some(&viewer),
            Some(http_monitor("x"))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "PATCH",
            &item,
            Some(&viewer),
            Some(json!({ "paused": true }))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "DELETE", &item, Some(&viewer), None).await.0,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test]
async fn non_member_gets_403_never_404(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let collection = format!("/projects/{project}/monitors");
    let existing = format!("{collection}/{monitor}");
    let missing = format!("{collection}/{}", Uuid::new_v4());

    assert_eq!(
        call(&app, "GET", &collection, Some(&outsider), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &collection,
            Some(&outsider),
            Some(http_monitor("x"))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for uri in [&existing, &missing] {
        assert_eq!(
            call(&app, "GET", uri, Some(&outsider), None).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(
                &app,
                "PATCH",
                uri,
                Some(&outsider),
                Some(json!({ "paused": true }))
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&app, "DELETE", uri, Some(&outsider), None).await.0,
            StatusCode::FORBIDDEN
        );
    }
}

#[sqlx::test]
async fn monitor_of_another_project_is_404_inside_an_accessible_project(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project_a = seed_project(&pool).await;
    let project_b = seed_project(&pool).await;
    let foreign =
        seed_http_monitor(&pool, project_b, "B's monitor", "https://example.com", 2).await;
    let (_, _, admin_a) = user_with_role(&app, &pool, project_a, Some("admin")).await;
    let uri = format!("/projects/{project_a}/monitors/{foreign}");

    assert_eq!(
        call(&app, "GET", &uri, Some(&admin_a), None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "PATCH",
            &uri,
            Some(&admin_a),
            Some(json!({ "name": "hijacked" }))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "DELETE", &uri, Some(&admin_a), None).await.0,
        StatusCode::NOT_FOUND
    );
    let (_, list) = call(
        &app,
        "GET",
        &format!("/projects/{project_a}/monitors"),
        Some(&admin_a),
        None,
    )
    .await;
    assert_eq!(list, json!([]));

    let (name, project): (String, Uuid) =
        sqlx::query_as("select name, project_id from monitors where id = $1")
            .bind(foreign)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        (name.as_str(), project),
        ("B's monitor", project_b),
        "B's monitor is untouched"
    );
}

/// Review Focus: a keyword monitor with HEAD (no body to search) is
/// rejected instead of silently reporting DOWN forever.
#[sqlx::test]
async fn invalid_configs_are_rejected_with_400_and_nothing_is_written(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let url = "https://example.com";
    let cases = [
        (
            json!({ "name": "x", "check_type": "icmp", "url": url }),
            "check_type",
        ),
        (
            json!({ "name": "  ", "check_type": "http", "url": url }),
            "name",
        ),
        (
            json!({ "name": "a\r\nBcc: x@evil.test", "check_type": "http", "url": url }),
            "control",
        ),
        (json!({ "name": "x", "check_type": "http" }), "url"),
        (
            json!({ "name": "x", "check_type": "http", "url": "ftp://example.com" }),
            "url",
        ),
        (
            json!({ "name": "x", "check_type": "http", "url": "not a url" }),
            "url",
        ),
        (
            json!({ "name": "x", "check_type": "http", "url": url, "http_method": "TRACE" }),
            "http_method",
        ),
        (
            json!({ "name": "x", "check_type": "keyword", "url": url }),
            "keyword",
        ),
        (
            json!({ "name": "x", "check_type": "keyword", "url": url, "keyword": "   " }),
            "keyword",
        ),
        (
            json!({ "name": "x", "check_type": "keyword", "url": url, "keyword": "ok", "http_method": "HEAD" }),
            "GET",
        ),
        (
            json!({ "name": "x", "check_type": "tcp", "port": 5432 }),
            "host",
        ),
        (
            json!({ "name": "x", "check_type": "tcp", "host": "db" }),
            "port",
        ),
        (
            json!({ "name": "x", "check_type": "tcp", "host": "db", "port": 0 }),
            "port",
        ),
        (
            json!({ "name": "x", "check_type": "tcp", "host": "db", "port": 65536 }),
            "port",
        ),
        (
            json!({ "name": "x", "check_type": "http", "url": url, "interval_seconds": 29 }),
            "interval_seconds",
        ),
        (
            json!({ "name": "x", "check_type": "http", "url": url, "timeout_seconds": 0 }),
            "timeout_seconds",
        ),
        (
            json!({ "name": "x", "check_type": "http", "url": url, "timeout_seconds": 61 }),
            "timeout_seconds",
        ),
        (
            json!({ "name": "x", "check_type": "http", "url": url, "expected_status_min": 400, "expected_status_max": 300 }),
            "expected_status_min",
        ),
        (
            json!({ "name": "x", "check_type": "http", "url": url, "expected_status_max": 600 }),
            "between 100 and 599",
        ),
        (
            json!({ "name": "x", "check_type": "http", "url": url, "confirmation_threshold": 0 }),
            "confirmation_threshold",
        ),
        (
            json!({ "name": "x", "check_type": "http", "url": url, "confirmation_threshold": 11 }),
            "confirmation_threshold",
        ),
    ];
    for (body, fragment) in cases {
        let (status, json) = call(
            &app,
            "POST",
            &format!("/projects/{project}/monitors"),
            Some(&member),
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} returned {json}");
        let message = json["error"].as_str().unwrap_or_default();
        assert!(
            message.contains(fragment),
            "{body}: error {message:?} should mention {fragment:?}"
        );
    }
    let count: i64 = sqlx::query_scalar("select count(*) from monitors")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "no invalid monitor was written");
}

#[sqlx::test]
async fn invalid_patch_is_rejected_and_leaves_the_monitor_unchanged(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let uri = format!("/projects/{project}/monitors/{monitor}");

    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        Some(&member),
        Some(json!({ "check_type": "tcp" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"].as_str().unwrap().contains("host"));
    let (status, _) = call(
        &app,
        "PATCH",
        &uri,
        Some(&member),
        Some(json!({ "interval_seconds": 10 })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (_, current) = call(&app, "GET", &uri, Some(&member), None).await;
    assert_eq!(current["check_type"], "http");
    assert_eq!(current["url"], "https://example.com");
    assert_eq!(current["interval_seconds"], 60);
}

#[sqlx::test]
async fn switching_check_type_clears_fields_of_the_old_type(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "DB", "https://example.com", 2).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let uri = format!("/projects/{project}/monitors/{monitor}");

    let (status, body) = call(
        &app,
        "PATCH",
        &uri,
        Some(&member),
        Some(json!({ "check_type": "tcp", "host": " db.internal ", "port": 5432 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["host"], "db.internal", "host is trimmed");
    assert_eq!(body["port"], 5432);
    assert_eq!(body["url"], Value::Null, "the http url no longer applies");
}

#[sqlx::test]
async fn editing_check_config_reschedules_now_but_renaming_does_not(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let uri = format!("/projects/{project}/monitors/{monitor}");
    sqlx::query("update monitors set next_check_at = now() + interval '1 hour' where id = $1")
        .bind(monitor)
        .execute(&pool)
        .await
        .unwrap();
    let due_now = || async {
        sqlx::query_scalar::<_, bool>("select next_check_at <= now() from monitors where id = $1")
            .bind(monitor)
            .fetch_one(&pool)
            .await
            .unwrap()
    };

    call(
        &app,
        "PATCH",
        &uri,
        Some(&member),
        Some(json!({ "name": "renamed" })),
    )
    .await;
    assert!(!due_now().await, "a rename keeps the schedule");

    let (status, _) = call(
        &app,
        "PATCH",
        &uri,
        Some(&member),
        Some(json!({ "url": "https://example.org/" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(due_now().await, "a new url is checked on the next tick");
}

#[sqlx::test]
async fn server_owned_fields_in_the_body_are_ignored(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other_project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;

    let mut body = http_monitor("API");
    body["project_id"] = json!(other_project);
    body["current_status"] = json!("up");
    body["consecutive_failures"] = json!(7);
    let (status, created) = call(
        &app,
        "POST",
        &format!("/projects/{project}/monitors"),
        Some(&member),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["project_id"], project.to_string());
    assert_eq!(created["current_status"], "pending");

    let uri = format!(
        "/projects/{project}/monitors/{}",
        created["id"].as_str().unwrap()
    );
    let (status, patched) = call(
        &app,
        "PATCH",
        &uri,
        Some(&member),
        Some(json!({ "name": "x", "project_id": other_project, "current_status": "down", "consecutive_failures": 9 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(patched["project_id"], project.to_string());
    assert_eq!(patched["current_status"], "pending");
    assert_eq!(patched["consecutive_failures"], 0);
}

// Validation hardening (fix round 1): rules for urls, hosts, keywords, names
// and the timeout/interval relation, each shown at the API level to answer
// 400 and to write nothing (create) or change nothing (PATCH).

/// Number of monitors in the database, to show that a rejected request wrote
/// nothing.
async fn monitor_count(pool: &sqlx::PgPool) -> i64 {
    sqlx::query_scalar("select count(*) from monitors")
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Sends one request and expects a 400 whose message contains `fragment`.
/// `label` names the case in a failure (some bodies are thousands of bytes).
async fn expect_400(
    app: &Router,
    method: &str,
    uri: &str,
    cookie: &HeaderValue,
    label: &str,
    body: Value,
    fragment: &str,
) {
    let (status, json) = call(app, method, uri, Some(cookie), Some(body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{label} returned {json}");
    let message = json["error"].as_str().unwrap_or_default();
    assert!(
        message.contains(fragment),
        "{label}: error {message:?} should mention {fragment:?}"
    );
}

/// PATCHes `body`, expects a 400 mentioning `fragment`, and expects the
/// monitor to read back exactly as it did before.
async fn expect_patch_rejected(
    app: &Router,
    item: &str,
    cookie: &HeaderValue,
    label: &str,
    body: Value,
    fragment: &str,
) {
    let before = call(app, "GET", item, Some(cookie), None).await.1;
    expect_400(app, "PATCH", item, cookie, label, body, fragment).await;
    let after = call(app, "GET", item, Some(cookie), None).await.1;
    assert_eq!(
        after, before,
        "{label}: a rejected PATCH must leave the monitor unchanged"
    );
}

#[sqlx::test]
async fn url_with_whitespace_control_characters_or_over_2048_bytes_is_rejected(pool: sqlx::PgPool) {
    const CLEAN: &str = "whitespace or control characters";
    const TOO_LONG: &str = "at most 2048 bytes";
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/monitors");
    let create = |url: &str| json!({ "name": "x", "check_type": "http", "url": url });
    // "https://example.com/" is 20 bytes; U+00E9 is 2 bytes in UTF-8.
    let padded = |bytes: usize| format!("https://example.com/{}", "a".repeat(bytes - 20));
    let two_byte =
        |bytes: usize| format!("https://example.com/{}", "\u{e9}".repeat((bytes - 20) / 2));

    let cases = [
        ("space", "https://example.com/a b".to_string(), CLEAN),
        ("tab", "https://example.com/a\tb".to_string(), CLEAN),
        (
            "newline inside the host",
            "https://exa\nmple.com/".to_string(),
            CLEAN,
        ),
        (
            "CRLF",
            "https://example.com/\r\nHost: evil.test".to_string(),
            CLEAN,
        ),
        ("NUL", "https://example.com/a\u{0}b".to_string(), CLEAN),
        ("BEL", "https://example.com/a\u{7}b".to_string(), CLEAN),
        ("DEL", "https://example.com/a\u{7f}b".to_string(), CLEAN),
        (
            "C1 control (NEL)",
            "https://example.com/a\u{85}b".to_string(),
            CLEAN,
        ),
        (
            "no-break space",
            "https://example.com/a\u{a0}b".to_string(),
            CLEAN,
        ),
        (
            "ideographic space",
            "https://example.com/a\u{3000}b".to_string(),
            CLEAN,
        ),
        ("2049 bytes", padded(2049), TOO_LONG),
        (
            "2050 bytes of two-byte characters",
            two_byte(2050),
            TOO_LONG,
        ),
    ];
    for (label, url, fragment) in cases {
        expect_400(
            &app,
            "POST",
            &collection,
            &member,
            label,
            create(&url),
            fragment,
        )
        .await;
    }
    assert_eq!(monitor_count(&pool).await, 0, "no rejected url was written");

    for (label, url) in [
        ("exactly 2048 bytes", padded(2048)),
        ("exactly 2048 bytes of two-byte characters", two_byte(2048)),
    ] {
        let (status, created) =
            call(&app, "POST", &collection, Some(&member), Some(create(&url))).await;
        assert_eq!(status, StatusCode::CREATED, "{label}: {created}");
        assert_eq!(created["url"], url.as_str(), "{label}");
    }
    // Surrounding whitespace is trimmed (as for names and hosts), so what is
    // stored never has any.
    let (status, trimmed) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(create("  https://example.com/x\n")),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{trimmed}");
    assert_eq!(trimmed["url"], "https://example.com/x");

    let item = format!("{collection}/{}", trimmed["id"].as_str().unwrap());
    let with_space = json!({ "url": "https://example.com/a b" });
    expect_patch_rejected(
        &app,
        &item,
        &member,
        "PATCH url with a space",
        with_space,
        CLEAN,
    )
    .await;
    let too_long = json!({ "url": padded(2049) });
    expect_patch_rejected(
        &app,
        &item,
        &member,
        "PATCH url of 2049 bytes",
        too_long,
        TOO_LONG,
    )
    .await;
}

#[sqlx::test]
async fn tcp_host_must_be_a_bare_hostname_or_ip_of_at_most_253_characters(pool: sqlx::PgPool) {
    const BARE: &str = "host must not contain";
    const PORT: &str = "port field";
    const TOO_LONG: &str = "at most 253 characters";
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/monitors");
    let tcp = |host: &str| json!({ "name": "db", "check_type": "tcp", "host": host, "port": 5432 });
    let two_byte = "\u{e9}"; // 2 bytes in UTF-8, 1 character

    let cases = [
        ("space", "db internal".to_string(), BARE),
        ("tab", "db\tinternal".to_string(), BARE),
        ("newline", "db\ninternal".to_string(), BARE),
        ("NUL", "db\u{0}".to_string(), BARE),
        ("no-break space", "db\u{a0}internal".to_string(), BARE),
        ("scheme", "tcp://db".to_string(), BARE),
        ("path", "db/path".to_string(), BARE),
        ("query", "db?x=1".to_string(), BARE),
        ("fragment", "db#x".to_string(), BARE),
        ("credentials", "user@db".to_string(), BARE),
        ("port in the host", "db:5432".to_string(), PORT),
        ("bracketed IPv6", "[::1]".to_string(), PORT),
        ("254 characters", "a".repeat(254), TOO_LONG),
        ("254 two-byte characters", two_byte.repeat(254), TOO_LONG),
    ];
    for (label, host, fragment) in cases {
        expect_400(
            &app,
            "POST",
            &collection,
            &member,
            label,
            tcp(&host),
            fragment,
        )
        .await;
    }
    assert_eq!(
        monitor_count(&pool).await,
        0,
        "no rejected host was written"
    );

    for (label, host) in [
        ("hostname", "db.internal".to_string()),
        ("IPv4", "10.0.0.5".to_string()),
        ("IPv6 loopback", "::1".to_string()),
        ("IPv6", "2001:db8::1".to_string()),
        ("exactly 253 characters", "a".repeat(253)),
        ("253 two-byte characters", two_byte.repeat(253)),
        ("surrounding whitespace", "  db.example.com\t".to_string()),
    ] {
        let (status, created) =
            call(&app, "POST", &collection, Some(&member), Some(tcp(&host))).await;
        assert_eq!(status, StatusCode::CREATED, "{label}: {created}");
        assert_eq!(created["host"], host.trim(), "{label}");
    }

    let (_, http) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(http_monitor("API")),
    )
    .await;
    let item = format!("{collection}/{}", http["id"].as_str().unwrap());
    let credentials = json!({ "check_type": "tcp", "host": "user@db", "port": 5432 });
    expect_patch_rejected(
        &app,
        &item,
        &member,
        "PATCH to tcp, credentials in the host",
        credentials,
        BARE,
    )
    .await;
    let with_port = json!({ "check_type": "tcp", "host": "db:5432", "port": 5432 });
    expect_patch_rejected(
        &app,
        &item,
        &member,
        "PATCH to tcp, port in the host",
        with_port,
        PORT,
    )
    .await;
}

#[sqlx::test]
async fn keyword_must_not_contain_control_characters_or_exceed_1024_bytes(pool: sqlx::PgPool) {
    const CONTROL: &str = "keyword must not contain control characters";
    const TOO_LONG: &str = "at most 1024 bytes";
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/monitors");
    let keyword_monitor = |keyword: &str| json!({ "name": "home", "check_type": "keyword", "url": "https://example.com", "keyword": keyword });
    let two_byte = "\u{e9}"; // 2 bytes in UTF-8, 1 character

    let cases = [
        ("tab", "a\tb".to_string(), CONTROL),
        ("newline", "a\nb".to_string(), CONTROL),
        ("NUL", "a\u{0}b".to_string(), CONTROL),
        ("DEL", "a\u{7f}b".to_string(), CONTROL),
        ("1025 bytes", "k".repeat(1025), TOO_LONG),
        (
            "513 two-byte characters (1026 bytes)",
            two_byte.repeat(513),
            TOO_LONG,
        ),
    ];
    for (label, keyword, fragment) in cases {
        expect_400(
            &app,
            "POST",
            &collection,
            &member,
            label,
            keyword_monitor(&keyword),
            fragment,
        )
        .await;
    }
    assert_eq!(
        monitor_count(&pool).await,
        0,
        "no rejected keyword was written"
    );

    for (label, keyword) in [
        ("exactly 1024 bytes", "k".repeat(1024)),
        ("512 two-byte characters (1024 bytes)", two_byte.repeat(512)),
        (
            "inner spaces and non-ASCII",
            "Selamat datang \u{2615}".to_string(),
        ),
    ] {
        let (status, created) = call(
            &app,
            "POST",
            &collection,
            Some(&member),
            Some(keyword_monitor(&keyword)),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{label}: {created}");
        assert_eq!(created["keyword"], keyword.as_str(), "{label}");
    }

    let (_, existing) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(keyword_monitor("Welcome")),
    )
    .await;
    let item = format!("{collection}/{}", existing["id"].as_str().unwrap());
    let with_tab = json!({ "keyword": "a\tb" });
    expect_patch_rejected(
        &app,
        &item,
        &member,
        "PATCH keyword with a tab",
        with_tab,
        CONTROL,
    )
    .await;
    let too_long = json!({ "keyword": "k".repeat(1025) });
    expect_patch_rejected(
        &app,
        &item,
        &member,
        "PATCH keyword of 1025 bytes",
        too_long,
        TOO_LONG,
    )
    .await;
}

#[sqlx::test]
async fn names_with_invisible_or_text_direction_characters_are_rejected(pool: sqlx::PgPool) {
    const INVISIBLE: &str = "invisible or text-direction characters";
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/monitors");
    let named =
        |name: &str| json!({ "name": name, "check_type": "http", "url": "https://example.com" });

    // Both ends of every rejected range, plus the single characters. (The
    // zero-width non-joiner and joiner, U+200C/U+200D, are allowed: see below.)
    let chars = [
        '\u{00AD}', '\u{034F}', '\u{061C}', '\u{115F}', '\u{1160}', '\u{180E}', '\u{200B}',
        '\u{200E}', '\u{200F}', '\u{2028}', '\u{2029}', '\u{202A}', '\u{202D}', '\u{202E}',
        '\u{2060}', '\u{2064}', '\u{2066}', '\u{2069}', '\u{206A}', '\u{206F}', '\u{2800}',
        '\u{3164}', '\u{FEFF}', '\u{FFA0}',
    ];
    for c in chars {
        let label = format!("U+{:04X} inside a name", c as u32);
        let body = named(&format!("pay{c}ments"));
        expect_400(&app, "POST", &collection, &member, &label, body, INVISIBLE).await;
    }
    // At the edges and alone, where trimming must not hide them.
    for (label, name) in [
        ("zero-width space at the start", "\u{200B}Payments"),
        ("line separator at the end", "Payments\u{2028}"),
        ("only a zero-width space", "\u{200B}"),
    ] {
        expect_400(
            &app,
            "POST",
            &collection,
            &member,
            label,
            named(name),
            INVISIBLE,
        )
        .await;
    }
    assert_eq!(
        monitor_count(&pool).await,
        0,
        "no rejected name was written"
    );

    let (status, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(named(
            "Layanan Pembayaran \u{2615} \u{65e5}\u{672c}\u{8a9e}",
        )),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "ordinary non-ASCII names are fine: {created}"
    );
    // Persian with a zero-width non-joiner, and an emoji joined by a ZWJ.
    for name in [
        "\u{645}\u{6cc}\u{200c}\u{62e}\u{648}\u{627}\u{647}\u{645}",
        "\u{1f469}\u{200d}\u{1f4bb} on call",
    ] {
        let (status, body) =
            call(&app, "POST", &collection, Some(&member), Some(named(name))).await;
        assert_eq!(status, StatusCode::CREATED, "{name:?}: {body}");
        assert_eq!(body["name"], name);
    }

    let item = format!("{collection}/{}", created["id"].as_str().unwrap());
    let renamed = json!({ "name": "a\u{202E}b" });
    expect_patch_rejected(
        &app,
        &item,
        &member,
        "PATCH name with a right-to-left override",
        renamed,
        INVISIBLE,
    )
    .await;
}

#[sqlx::test]
async fn timeout_must_be_less_than_interval_on_create_and_on_the_merged_patch(pool: sqlx::PgPool) {
    const FRAGMENT: &str = "timeout_seconds must be less than interval_seconds";
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/monitors");
    let monitor = |interval: i32, timeout: i32| {
        json!({
            "name": "x", "check_type": "http", "url": "https://example.com",
            "interval_seconds": interval, "timeout_seconds": timeout,
        })
    };

    for (interval, timeout) in [(30, 30), (30, 31), (45, 45), (60, 60)] {
        let label = format!("interval {interval}, timeout {timeout}");
        expect_400(
            &app,
            "POST",
            &collection,
            &member,
            &label,
            monitor(interval, timeout),
            FRAGMENT,
        )
        .await;
    }
    // The default interval (60 s) leaves no room for a 60 s timeout.
    let default_interval = json!({
        "name": "x", "check_type": "http", "url": "https://example.com", "timeout_seconds": 60,
    });
    expect_400(
        &app,
        "POST",
        &collection,
        &member,
        "default interval, timeout 60",
        default_interval,
        FRAGMENT,
    )
    .await;
    assert_eq!(
        monitor_count(&pool).await,
        0,
        "no rejected monitor was written"
    );

    for (interval, timeout) in [(30, 29), (31, 30), (60, 59)] {
        let (status, created) = call(
            &app,
            "POST",
            &collection,
            Some(&member),
            Some(monitor(interval, timeout)),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "interval {interval}, timeout {timeout}: {created}"
        );
    }

    // PATCH validates the merged config, so a field the request does not
    // mention can still be the one that breaks the rule.
    let seeded = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await; // 60 s / 5 s
    let item = format!("{collection}/{seeded}");
    let up_to_interval = json!({ "timeout_seconds": 60 });
    expect_patch_rejected(
        &app,
        &item,
        &member,
        "PATCH timeout up to the interval",
        up_to_interval,
        FRAGMENT,
    )
    .await;
    sqlx::query("update monitors set timeout_seconds = 40 where id = $1")
        .bind(seeded)
        .execute(&pool)
        .await
        .unwrap();
    let under_timeout = json!({ "interval_seconds": 30 });
    expect_patch_rejected(
        &app,
        &item,
        &member,
        "PATCH interval 30 under a 40 s timeout",
        under_timeout,
        FRAGMENT,
    )
    .await;
    let both = json!({ "interval_seconds": 30, "timeout_seconds": 29 });
    let (status, patched) = call(&app, "PATCH", &item, Some(&member), Some(both)).await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["interval_seconds"], 30);
    assert_eq!(patched["timeout_seconds"], 29);
}

/// `update_in_project` merges a PATCH into the row it read, so without its
/// `for update` lock concurrent edits of different fields overwrite one
/// another and all but one change is silently lost. The fields below
/// validate independently of each other, so every interleaving is valid and
/// a missing change can only come from a lost update.
#[sqlx::test]
async fn concurrent_patches_of_different_fields_are_all_applied(pool: sqlx::PgPool) {
    const ROUNDS: usize = 10;
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let patches = [
        ("name", json!("renamed")),
        ("confirmation_threshold", json!(5)),
        ("paused", json!(true)),
        ("http_method", json!("HEAD")),
        ("keyword_mode", json!("not_contains")),
        ("interval_seconds", json!(120)),
        ("url", json!("https://example.org/")),
    ];

    for round in 0..ROUNDS {
        let monitor = seed_http_monitor(&pool, project, "before", "https://example.com", 2).await;
        let uri = format!("/projects/{project}/monitors/{monitor}");

        let mut requests = JoinSet::new();
        for (field, value) in &patches {
            let (app, uri, member) = (app.clone(), uri.clone(), member.clone());
            let body = Value::Object([(field.to_string(), value.clone())].into_iter().collect());
            requests
                .spawn(async move { call(&app, "PATCH", &uri, Some(&member), Some(body)).await });
        }
        while let Some(finished) = requests.join_next().await {
            let (status, body) = finished.unwrap();
            assert_eq!(status, StatusCode::OK, "round {round}: {body}");
        }

        let (_, current) = call(&app, "GET", &uri, Some(&member), None).await;
        let lost: Vec<&str> = patches
            .iter()
            .filter(|(field, value)| current[*field] != *value)
            .map(|(field, _)| *field)
            .collect();
        assert!(
            lost.is_empty(),
            "round {round}: concurrent PATCHes lost the update to {lost:?}"
        );
    }
}

/// The URL and the host follow the same rule as names: no invisible or
/// text-direction characters anywhere (they would make a target read as
/// another one), at the edges included, where trimming must not hide them.
/// The keyword may contain them: it is matched against page text, which can
/// hold anything.
#[sqlx::test]
async fn urls_and_hosts_with_invisible_or_text_direction_characters_are_rejected(
    pool: sqlx::PgPool,
) {
    const INVISIBLE: &str = "invisible or text-direction characters";
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/monitors");

    for (label, url) in [
        ("RLO in the path", "https://example.com/a\u{202E}b"),
        (
            "zero-width space in the host",
            "https://exa\u{200B}mple.com/",
        ),
        ("isolate in the query", "https://example.com/?q=\u{2066}x"),
        ("line separator at the end", "https://example.com/\u{2028}"),
        ("Hangul filler", "https://example.com/\u{3164}"),
        ("BOM at the start", "\u{FEFF}https://example.com/"),
    ] {
        let body = json!({ "name": "x", "check_type": "http", "url": url });
        expect_400(&app, "POST", &collection, &member, label, body, INVISIBLE).await;
    }
    for (label, host) in [
        ("zero-width space", "db\u{200B}.internal"),
        ("RLO", "db\u{202E}.internal"),
        ("paragraph separator at the start", "\u{2029}db.internal"),
        ("soft hyphen", "db\u{00AD}internal"),
    ] {
        let body = json!({ "name": "x", "check_type": "tcp", "host": host, "port": 5432 });
        expect_400(&app, "POST", &collection, &member, label, body, INVISIBLE).await;
    }
    assert_eq!(
        monitor_count(&pool).await,
        0,
        "nothing rejected was written"
    );

    let (status, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(json!({
            "name": "x",
            "check_type": "keyword",
            "url": "https://example.com/",
            "keyword": "\u{5e9}\u{5dc}\u{5d5}\u{5dd}\u{200F} \u{202E}ok",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "keywords may: {created}");

    let item = format!("{collection}/{}", created["id"].as_str().unwrap());
    expect_patch_rejected(
        &app,
        &item,
        &member,
        "PATCH url with a right-to-left override",
        json!({ "url": "https://example.com/\u{202E}" }),
        INVISIBLE,
    )
    .await;
}

/// Viewers read monitors with the secrets taken out of the URL: the userinfo
/// password, every query value and the fragment read `***`, while the scheme,
/// host, port, path and parameter names stay, in the list and in the detail.
/// Members, project admins and global admins get the URL as it was saved.
#[sqlx::test]
async fn viewers_get_monitor_urls_without_their_secrets(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let root_email = unique_email("root");
    seed_user(&pool, &root_email, "admin").await;
    let root = login(&app, &root_email).await;
    let collection = format!("/projects/{project}/monitors");
    let url = "https://alerts:s3cret-pw@status.example.com:8443/health?token=abc123&region=eu#frag-secret";
    let redacted = "https://alerts:***@status.example.com:8443/health?token=***&region=***#***";

    let (status, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(json!({ "name": "API", "check_type": "http", "url": url })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["url"], url, "the member who saved it sees it whole");
    let item = format!("{collection}/{}", created["id"].as_str().unwrap());

    let (status, detail) = call(&app, "GET", &item, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(detail["url"], redacted);
    let (status, list) = call(&app, "GET", &collection, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0]["url"], redacted);
    for body in [&detail, &list] {
        let text = body.to_string();
        for secret in ["s3cret-pw", "abc123", "frag-secret", "=eu"] {
            assert!(!text.contains(secret), "{secret} reached a viewer: {text}");
        }
    }
    assert_eq!(detail["name"], "API", "everything else is as saved");

    for (who, cookie) in [
        ("member", &member),
        ("project admin", &admin),
        ("global admin", &root),
    ] {
        let (_, detail) = call(&app, "GET", &item, Some(cookie), None).await;
        assert_eq!(detail["url"], url, "{who}");
        let (_, list) = call(&app, "GET", &collection, Some(cookie), None).await;
        assert_eq!(list[0]["url"], url, "{who}");
    }
}

/// A URL without secrets reads the same for a viewer, and a username on its
/// own (often a token, as in `https://<token>@host/`) is hidden too.
#[sqlx::test]
async fn a_viewer_sees_plain_urls_unchanged_and_a_lone_username_hidden(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let plain = seed_http_monitor(&pool, project, "plain", "https://example.com/health", 2).await;
    let token = seed_http_monitor(
        &pool,
        project,
        "token",
        "https://ghp_tok3n@example.com/x?",
        2,
    )
    .await;

    let (_, plain) = call(
        &app,
        "GET",
        &format!("/projects/{project}/monitors/{plain}"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(plain["url"], "https://example.com/health");
    let (_, token) = call(
        &app,
        "GET",
        &format!("/projects/{project}/monitors/{token}"),
        Some(&viewer),
        None,
    )
    .await;
    assert_eq!(token["url"], "https://***@example.com/x?");
}

/// Rows stored before the tag characters were refused stay editable: a PATCH
/// that leaves the name (or the URL) alone pauses the monitor, one that sets
/// a new name with a stray tag character is refused, and a subdivision flag
/// is a fine name.
#[sqlx::test]
async fn a_stored_stray_tag_character_never_blocks_other_edits(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let monitor = seed_http_monitor(&pool, project, "legacy", "https://example.com/", 1).await;
    sqlx::query(
        "update monitors set name = 'legacy\u{E0041}', url = 'https://example.com/?q=\u{E0041}' where id = $1",
    )
    .bind(monitor)
    .execute(&pool)
    .await
    .unwrap();
    let item = format!("/projects/{project}/monitors/{monitor}");

    let (status, body) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "paused": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["paused"], true);
    assert_eq!(body["name"], "legacy\u{E0041}");

    let (status, body) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "name": "new\u{E0041}" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let england = "\u{1F3F4}\u{E0067}\u{E0062}\u{E0065}\u{E006E}\u{E0067}\u{E007F}";
    let (status, body) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "name": format!("{england} checkout") })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], format!("{england} checkout"));
}
