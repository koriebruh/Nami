// Requires `docker compose up -d postgres`.
mod common;

use axum::http::StatusCode;
use common::{
    LogCapture, app, call, login, seed_http_monitor, seed_project, seed_user, test_app,
    unique_email, user_with_role,
};
use nami_engine::{
    core::crypto::{Cipher, DecryptError, SecretString, TEST_KEY},
    core::state::AppState,
    uptime::monitor::{
        model::{CreateMonitorRequest, Monitor, MonitorView, PatchMonitorRequest},
        request::{RequestHeader, open_headers, open_password},
    },
};
use serde_json::{Value, json};
use uuid::Uuid;

const HEADER_SECRET: &str = "header-secret-4f9a";
const PASSWORD_SECRET: &str = "password-secret-7c21";
/// Rides in the request body; only a member may read it.
const BODY_SECRET: &str = "body-secret-1d3f";
/// Rides in an invalid request: no error text may repeat it.
const LEAKY: &str = "leaky-8b52";

fn configured_monitor() -> Value {
    json!({
        "name": "Orders API",
        "check_type": "http",
        "url": "https://example.com/orders",
        "http_method": "POST",
        "request_body": "{\"probe\":true}",
        "request_headers": [
            { "name": "X-Api-Key", "value": HEADER_SECRET },
            { "name": "Content-Type", "value": "application/json" }
        ],
        "auth_username": "monitor",
        "auth_password": PASSWORD_SECRET,
        "follow_redirects": false,
        "verify_ssl": false
    })
}

/// Fails if a secret, or any `*_enc` column, appears anywhere in `body`.
fn assert_redacted(body: &Value) {
    let text = body.to_string();
    for secret in [HEADER_SECRET, PASSWORD_SECRET] {
        assert!(!text.contains(secret), "secret leaked in {text}");
    }
    assert!(!text.contains("_enc"), "ciphertext column leaked in {text}");
    assert!(
        !text.contains("request_header_names"),
        "internal column leaked in {text}"
    );
}

type StoredSecrets = (Option<Vec<u8>>, Vec<String>, Option<Vec<u8>>);

async fn stored_secrets(pool: &sqlx::PgPool, id: &str) -> StoredSecrets {
    sqlx::query_as(
        "select request_headers_enc, request_header_names, auth_password_enc from monitors where id = $1",
    )
    .bind(Uuid::parse_str(id).unwrap())
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn make_not_due(pool: &sqlx::PgPool, id: &str) {
    sqlx::query("update monitors set next_check_at = now() + interval '1 hour' where id = $1")
        .bind(Uuid::parse_str(id).unwrap())
        .execute(pool)
        .await
        .unwrap();
}

async fn is_due(pool: &sqlx::PgPool, id: &str) -> bool {
    sqlx::query_scalar("select next_check_at <= now() from monitors where id = $1")
        .bind(Uuid::parse_str(id).unwrap())
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn request_config_round_trips_and_every_response_redacts_secrets(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/monitors");

    let (status, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(configured_monitor()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_redacted(&created);
    assert_eq!(created["http_method"], "POST");
    assert_eq!(created["request_body"], "{\"probe\":true}");
    assert_eq!(
        created["request_headers"],
        json!([{ "name": "X-Api-Key", "value_set": true }, { "name": "Content-Type", "value_set": true }])
    );
    assert_eq!(created["auth_username"], "monitor");
    assert_eq!(created["has_auth_password"], true);
    assert_eq!(
        (
            created["follow_redirects"].clone(),
            created["verify_ssl"].clone()
        ),
        (json!(false), json!(false))
    );
    let item = format!("{collection}/{}", created["id"].as_str().unwrap());

    let (_, detail) = call(&app, "GET", &item, Some(&member), None).await;
    assert_redacted(&detail);
    assert_eq!(detail["request_headers"], created["request_headers"]);
    let (_, list) = call(&app, "GET", &collection, Some(&member), None).await;
    assert_redacted(&list);
    assert_eq!(list[0]["has_auth_password"], true);
    assert_eq!(list[0]["request_headers"], created["request_headers"]);
    let (status, patched) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "name": "Orders" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_redacted(&patched);
}

#[sqlx::test]
async fn defaults_follow_redirects_and_verify_ssl(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let body = json!({ "name": "Home", "check_type": "http", "url": "https://example.com" });

    let (status, created) = call(
        &app,
        "POST",
        &format!("/projects/{project}/monitors"),
        Some(&member),
        Some(body),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["follow_redirects"], true);
    assert_eq!(created["verify_ssl"], true);
    assert_eq!(created["request_headers"], json!([]));
    assert_eq!(created["has_auth_password"], false);
    assert_eq!(created["request_body"], Value::Null);
    assert_eq!(created["auth_username"], Value::Null);
}

#[sqlx::test]
async fn secrets_are_stored_as_ciphertext_never_plaintext(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(
        &app,
        "POST",
        &format!("/projects/{project}/monitors"),
        Some(&member),
        Some(configured_monitor()),
    )
    .await;
    let id = created["id"].as_str().unwrap();

    let (headers_enc, names, password_enc) = stored_secrets(&pool, id).await;
    let (headers_enc, password_enc) = (headers_enc.unwrap(), password_enc.unwrap());
    for (sealed, secret) in [
        (&headers_enc, HEADER_SECRET),
        (&headers_enc, "X-Api-Key"),
        (&password_enc, PASSWORD_SECRET),
    ] {
        assert!(
            !sealed.windows(secret.len()).any(|w| w == secret.as_bytes()),
            "plaintext {secret} stored"
        );
    }
    assert_eq!(
        names,
        vec!["X-Api-Key", "Content-Type"],
        "only the names are kept in the clear"
    );

    // The ciphertext opens with the configured key: it is encrypted, not lost.
    let (cipher, id) = (Cipher::new(&TEST_KEY), Uuid::parse_str(id).unwrap());
    let headers = open_headers(&cipher, id, &headers_enc).unwrap();
    assert_eq!(headers[0].name, "X-Api-Key");
    assert_eq!(headers[0].value.expose(), HEADER_SECRET);
    assert_eq!(headers[1].value.expose(), "application/json");
    assert_eq!(
        open_password(&cipher, id, &password_enc).unwrap().expose(),
        PASSWORD_SECRET
    );
}

/// Every secret is sealed for its own column and its own monitor: a blob
/// copied onto another row, or opened as the other secret, or under another
/// key, does not open.
#[sqlx::test]
async fn sealed_secrets_open_only_for_their_own_monitor_column_and_key(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let mut ids = Vec::new();
    for name in ["A", "B"] {
        let mut body = configured_monitor();
        body["name"] = json!(name);
        let (status, created) = call(
            &app,
            "POST",
            &format!("/projects/{project}/monitors"),
            Some(&member),
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
        ids.push(created["id"].as_str().unwrap().to_string());
    }
    let (a, b) = (
        Uuid::parse_str(&ids[0]).unwrap(),
        Uuid::parse_str(&ids[1]).unwrap(),
    );
    let (headers_enc, _, password_enc) = stored_secrets(&pool, &ids[0]).await;
    let (headers_enc, password_enc) = (headers_enc.unwrap(), password_enc.unwrap());
    let cipher = Cipher::new(&TEST_KEY);

    assert!(open_headers(&cipher, a, &headers_enc).is_ok());
    assert!(open_password(&cipher, a, &password_enc).is_ok());
    assert_eq!(
        open_headers(&cipher, b, &headers_enc).unwrap_err(),
        DecryptError,
        "A's headers copied onto B must not open"
    );
    assert_eq!(
        open_password(&cipher, b, &password_enc).unwrap_err(),
        DecryptError,
        "A's password copied onto B must not open"
    );
    assert!(
        open_password(&cipher, a, &headers_enc).is_err(),
        "the headers column is not the password column"
    );
    assert!(open_headers(&cipher, a, &password_enc).is_err());
    assert!(open_headers(&Cipher::new(&[9u8; 32]), a, &headers_enc).is_err());

    // The same through the database: B's row carrying A's blob stays
    // unreadable for B, whatever the API says about it.
    sqlx::query("update monitors set request_headers_enc = $2 where id = $1")
        .bind(b)
        .bind(&headers_enc)
        .execute(&pool)
        .await
        .unwrap();
    let (copied, _, _) = stored_secrets(&pool, &ids[1]).await;
    assert!(open_headers(&cipher, b, &copied.unwrap()).is_err());
}

#[sqlx::test]
async fn patch_keeps_omitted_secrets_replaces_given_ones_and_null_clears(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(
        &app,
        "POST",
        &format!("/projects/{project}/monitors"),
        Some(&member),
        Some(configured_monitor()),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    let item = format!("/projects/{project}/monitors/{id}");
    let uuid = Uuid::parse_str(&id).unwrap();
    let before = stored_secrets(&pool, &id).await;

    let (status, _) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "name": "Renamed", "paused": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        stored_secrets(&pool, &id).await,
        before,
        "omitted secrets are kept byte for byte"
    );

    let (status, patched) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "auth_password": "rotated-pass", "request_headers": [{ "name": "X-Other", "value": "v2" }] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(
        patched["request_headers"],
        json!([{ "name": "X-Other", "value_set": true }])
    );
    let (headers_enc, names, password_enc) = stored_secrets(&pool, &id).await;
    let cipher = Cipher::new(&TEST_KEY);
    assert_eq!(
        open_password(&cipher, uuid, &password_enc.unwrap())
            .unwrap()
            .expose(),
        "rotated-pass"
    );
    assert_eq!(
        open_headers(&cipher, uuid, &headers_enc.unwrap()).unwrap()[0]
            .value
            .expose(),
        "v2"
    );
    assert_eq!(names, vec!["X-Other"]);

    let (status, cleared) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "request_headers": null, "auth_password": null })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(cleared["request_headers"], json!([]));
    assert_eq!(cleared["has_auth_password"], false);
    assert_eq!(
        cleared["auth_username"], "monitor",
        "the username is not a secret and stays"
    );
    assert_eq!(stored_secrets(&pool, &id).await, (None, Vec::new(), None));
}

/// Headers are replaced as a whole: `[]` clears them like `null` does, and a
/// body or a username clears the same way.
#[sqlx::test]
async fn an_empty_list_clears_headers_and_null_clears_body_and_username(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(
        &app,
        "POST",
        &format!("/projects/{project}/monitors"),
        Some(&member),
        Some(configured_monitor()),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    let item = format!("/projects/{project}/monitors/{id}");

    let (status, emptied) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "request_headers": [] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{emptied}");
    assert_eq!(emptied["request_headers"], json!([]));
    let (headers_enc, names, password_enc) = stored_secrets(&pool, &id).await;
    assert_eq!((headers_enc, names), (None, Vec::new()));
    assert!(password_enc.is_some(), "the password is not touched");

    // No username: no basic auth, so the password goes with it.
    let (status, no_auth) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "auth_username": null, "request_body": null })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{no_auth}");
    assert_eq!(no_auth["auth_username"], Value::Null);
    assert_eq!(no_auth["request_body"], Value::Null);
    assert_eq!(no_auth["has_auth_password"], false);
    assert_eq!(stored_secrets(&pool, &id).await, (None, Vec::new(), None));
}

#[sqlx::test]
async fn every_http_method_is_accepted_and_keyword_monitors_still_refuse_head(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/monitors");
    for method in ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
        let body = json!({ "name": method, "check_type": "http", "url": "https://example.com", "http_method": method });
        let (status, created) = call(&app, "POST", &collection, Some(&member), Some(body)).await;
        assert_eq!(status, StatusCode::CREATED, "{method}: {created}");
        assert_eq!(created["http_method"], method);
    }
    let keyword_post = json!({
        "name": "search", "check_type": "keyword", "url": "https://example.com/search",
        "keyword": "results", "http_method": "POST", "request_body": "q=nami"
    });
    assert_eq!(
        call(&app, "POST", &collection, Some(&member), Some(keyword_post))
            .await
            .0,
        StatusCode::CREATED
    );

    // A HEAD response has no body to search: refused, and the message says
    // what to use instead.
    let keyword_head = json!({
        "name": "head", "check_type": "keyword", "url": "https://example.com/search",
        "keyword": "results", "http_method": "HEAD"
    });
    let (status, refused) =
        call(&app, "POST", &collection, Some(&member), Some(keyword_head)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
    assert!(refused["error"].as_str().unwrap().contains("GET"));
    // Also when a PATCH turns an existing keyword monitor into a HEAD one.
    let keyword_get = json!({
        "name": "get", "check_type": "keyword", "url": "https://example.com/search",
        "keyword": "results"
    });
    let (_, created) = call(&app, "POST", &collection, Some(&member), Some(keyword_get)).await;
    let (status, refused) = call(
        &app,
        "PATCH",
        &format!("{collection}/{}", created["id"].as_str().unwrap()),
        Some(&member),
        Some(json!({ "http_method": "HEAD" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
}

/// Security: header injection and every other invalid request config is a
/// 400 that writes nothing.
#[sqlx::test]
async fn header_injection_and_invalid_request_config_are_rejected_and_nothing_is_stored(
    pool: sqlx::PgPool,
) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let base = |extra: Value| {
        let mut body = json!({ "name": "x", "check_type": "http", "url": "https://example.com", "http_method": "POST" });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };
    let header =
        |name: &str, value: &str| json!({ "request_headers": [{ "name": name, "value": value }] });
    let too_many: Vec<Value> = (0..21)
        .map(|i| json!({ "name": format!("X-H{i}"), "value": "v" }))
        .collect();
    let cases = [
        (header("X-Api-Key", "abc\r\nX-Injected: 1"), "CR or LF"),
        (header("X-Api-Key", "abc\nX-Injected: 1"), "CR or LF"),
        (header("X-Api-Key", "abc\rdef"), "CR or LF"),
        (header("X-Api-Key", "nul\u{0}byte"), "control characters"),
        (header("X-Api-Key", "del\u{7f}byte"), "control characters"),
        (header("X-Bad\r\nName", "v"), "not a valid HTTP header name"),
        (header("X Bad", "v"), "not a valid HTTP header name"),
        (header("X-Bad:", "v"), "not a valid HTTP header name"),
        (header("", "v"), "not a valid HTTP header name"),
        (header("   ", "v"), "not a valid HTTP header name"),
        (header("X-Ünï", "v"), "not a valid HTTP header name"),
        // A "Name: value" pasted into the name field must not echo the value.
        (
            header(&format!("Authorization: Bearer {LEAKY}"), "v"),
            "not a valid HTTP header name",
        ),
        (header(&"X".repeat(257), "v"), "at most 256 bytes"),
        (header("Host", "evil.test"), "cannot be set"),
        (header("content-length", "0"), "cannot be set"),
        (header("Transfer-Encoding", "chunked"), "cannot be set"),
        (header("CONNECTION", "close"), "cannot be set"),
        (header("  host  ", "evil.test"), "cannot be set"),
        (header("X-Long", &"a".repeat(1025)), "at most 1024 bytes"),
        (header("X-Long", &"é".repeat(513)), "at most 1024 bytes"),
        (json!({ "request_headers": too_many }), "at most 20"),
        (json!({ "http_method": "TRACE" }), "http_method"),
        (json!({ "http_method": "post" }), "http_method"),
        (
            json!({ "http_method": "GET", "request_body": "x" }),
            "only allowed with POST, PUT or PATCH",
        ),
        (
            json!({ "http_method": "HEAD", "request_body": "x" }),
            "only allowed with POST, PUT or PATCH",
        ),
        (
            json!({ "http_method": "DELETE", "request_body": "x" }),
            "only allowed with POST, PUT or PATCH",
        ),
        (
            json!({ "request_body": "a".repeat(64 * 1024 + 1) }),
            "at most 65536 bytes",
        ),
        (
            json!({ "request_body": "é".repeat(32 * 1024 + 1) }),
            "at most 65536 bytes",
        ),
        // Postgres text cannot hold NUL: a 400, not a 500.
        (json!({ "request_body": "a\u{0}b" }), "NUL"),
        (
            json!({ "auth_username": "a:b", "auth_password": "p" }),
            "auth_username",
        ),
        (
            json!({ "auth_username": "a\nb", "auth_password": "p" }),
            "auth_username",
        ),
        (
            json!({ "auth_username": "a\u{200b}b", "auth_password": "p" }),
            "invisible or text-direction characters",
        ),
        (
            json!({ "auth_username": "u".repeat(257), "auth_password": "p" }),
            "at most 256 characters",
        ),
        (
            json!({ "auth_username": "a", "auth_password": "p\r\nq" }),
            "auth_password",
        ),
        (
            json!({ "auth_username": "a", "auth_password": format!("{LEAKY}\u{0}") }),
            "auth_password must not contain control characters",
        ),
        (
            json!({ "auth_username": "a", "auth_password": "p".repeat(1025) }),
            "at most 1024 bytes",
        ),
        (
            json!({ "auth_password": "orphan" }),
            "auth_password requires auth_username",
        ),
        (
            json!({ "auth_username": "a", "auth_password": "p", "request_headers": [{ "name": "authorization", "value": "Bearer t" }] }),
            "either basic auth or an Authorization header",
        ),
    ];
    for (extra, fragment) in cases {
        let body = base(extra);
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
            "error {message:?} should mention {fragment:?}"
        );
        for secret in ["abc", "X-Injected", LEAKY, "evil.test"] {
            assert!(
                !message.contains(secret),
                "the error must not echo {secret}: {message}"
            );
        }
    }
    let count: i64 = sqlx::query_scalar("select count(*) from monitors")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "no invalid monitor was written");

    // The same checks guard PATCH: the stored monitor stays untouched.
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (status, _) = call(
        &app,
        "PATCH",
        &format!("/projects/{project}/monitors/{monitor}"),
        Some(&member),
        Some(header("X-Api-Key", "abc\r\nX-Injected: 1")),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let names: Vec<String> =
        sqlx::query_scalar("select request_header_names from monitors where id = $1")
            .bind(monitor)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(names.is_empty());
}

/// Limits are inclusive: a request exactly at each of them is accepted, whole.
#[sqlx::test]
async fn values_exactly_at_every_limit_are_accepted(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let headers: Vec<Value> = (0..20)
        .map(|i| json!({ "name": format!("X-H{i}"), "value": "v".repeat(1024) }))
        .collect();
    let body = json!({
        "name": "limits", "check_type": "http", "url": "https://example.com",
        "http_method": "PUT",
        "request_body": "a".repeat(64 * 1024),
        "request_headers": headers,
        "auth_username": "u".repeat(256),
        "auth_password": "p".repeat(1024),
    });

    let (status, created) = call(
        &app,
        "POST",
        &format!("/projects/{project}/monitors"),
        Some(&member),
        Some(body),
    )
    .await;

    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["request_headers"].as_array().unwrap().len(), 20);
    assert_eq!(created["request_body"].as_str().unwrap().len(), 64 * 1024);
    let (sealed, names, _) = stored_secrets(&pool, created["id"].as_str().unwrap()).await;
    assert_eq!(names.len(), 20);
    let opened = open_headers(
        &Cipher::new(&TEST_KEY),
        Uuid::parse_str(created["id"].as_str().unwrap()).unwrap(),
        &sealed.unwrap(),
    )
    .unwrap();
    assert_eq!(opened.len(), 20);
    assert_eq!(opened[19].value.expose().len(), 1024);
}

/// What a PATCH merges with the stored monitor is validated as a whole: the
/// stored body, username and header names count as much as the new fields.
#[sqlx::test]
async fn a_patch_is_validated_against_what_is_stored(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/monitors");
    let (_, with_auth) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(configured_monitor()),
    )
    .await;
    let with_auth = format!("{collection}/{}", with_auth["id"].as_str().unwrap());
    let (status, with_header) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(json!({
            "name": "bearer", "check_type": "http", "url": "https://example.com",
            "request_headers": [{ "name": "Authorization", "value": "Bearer abc" }]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{with_header}");
    let with_header = format!("{collection}/{}", with_header["id"].as_str().unwrap());

    let expect =
        |label: &'static str, item: String, body: Value, status: StatusCode, said: &'static str| {
            let (app, member) = (app.clone(), member.clone());
            async move {
                let (got, json) = call(&app, "PATCH", &item, Some(&member), Some(body)).await;
                assert_eq!(got, status, "{label}: {json}");
                if status == StatusCode::BAD_REQUEST {
                    let message = json["error"].as_str().unwrap_or_default();
                    assert!(message.contains(said), "{label}: {message:?} vs {said:?}");
                }
                json
            }
        };

    // The stored body stays, so a method that cannot carry it is refused
    // until the body is cleared as well.
    expect(
        "method without a body",
        with_auth.clone(),
        json!({ "http_method": "GET" }),
        StatusCode::BAD_REQUEST,
        "only allowed with POST, PUT or PATCH",
    )
    .await;
    let cleared = expect(
        "method and body together",
        with_auth.clone(),
        json!({ "http_method": "GET", "request_body": null }),
        StatusCode::OK,
        "",
    )
    .await;
    assert_eq!(cleared["http_method"], "GET");
    assert_eq!(cleared["request_body"], Value::Null);

    // Basic auth is stored: an Authorization header cannot be added next to it.
    expect(
        "header next to stored basic auth",
        with_auth.clone(),
        json!({ "request_headers": [{ "name": "Authorization", "value": "Bearer x" }] }),
        StatusCode::BAD_REQUEST,
        "either basic auth or an Authorization header",
    )
    .await;
    // An Authorization header is stored (kept, so only its name is known):
    // basic auth cannot be added next to it either.
    expect(
        "basic auth next to a stored header",
        with_header.clone(),
        json!({ "auth_username": "monitor", "auth_password": "p" }),
        StatusCode::BAD_REQUEST,
        "either basic auth or an Authorization header",
    )
    .await;
    // Replacing the header list resolves the conflict in the same request.
    expect(
        "swap the header for basic auth",
        with_header.clone(),
        json!({ "request_headers": [], "auth_username": "monitor", "auth_password": "p" }),
        StatusCode::OK,
        "",
    )
    .await;
    // A password needs a username, stored or sent.
    let (_, plain) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(json!({ "name": "plain", "check_type": "http", "url": "https://example.com" })),
    )
    .await;
    expect(
        "password without a username",
        format!("{collection}/{}", plain["id"].as_str().unwrap()),
        json!({ "auth_password": "p" }),
        StatusCode::BAD_REQUEST,
        "auth_password requires auth_username",
    )
    .await;
}

/// Credentials in the url become an `Authorization` header, and basic auth
/// adds another: the two exclude each other, on create and on PATCH, whichever
/// of them is stored.
#[sqlx::test]
async fn basic_auth_and_credentials_in_the_url_exclude_each_other(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/monitors");
    let conflict = "use either basic auth or credentials in the url, not both";
    let monitor = |url: &str, extra: Value| {
        let mut body = json!({ "name": "x", "check_type": "http", "url": url });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };
    let basic = json!({ "auth_username": "monitor", "auth_password": PASSWORD_SECRET });

    // Both at once: refused, nothing stored, and no secret in the message.
    for url in [
        "https://user:url-secret-9d4e@example.com/",
        "https://url-secret-9d4e@example.com/",
    ] {
        let (status, json) = call(
            &app,
            "POST",
            &collection,
            Some(&member),
            Some(monitor(url, basic.clone())),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{url}: {json}");
        let message = json["error"].as_str().unwrap_or_default();
        assert_eq!(message, conflict, "{url}");
    }
    let count: i64 = sqlx::query_scalar("select count(*) from monitors")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);

    // Either one alone is fine.
    let (status, with_url) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(monitor(
            "https://user:url-secret-9d4e@example.com/",
            json!({}),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{with_url}");
    let with_url = format!("{collection}/{}", with_url["id"].as_str().unwrap());
    let (status, with_basic) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(monitor("https://example.com/", basic.clone())),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{with_basic}");
    let with_basic = format!("{collection}/{}", with_basic["id"].as_str().unwrap());

    // PATCH is judged on what is stored: neither can be added to the other.
    let (status, json) = call(&app, "PATCH", &with_url, Some(&member), Some(basic.clone())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    assert_eq!(json["error"], conflict);
    let (status, json) = call(
        &app,
        "PATCH",
        &with_basic,
        Some(&member),
        Some(json!({ "url": "https://user:url-secret-9d4e@example.com/" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    assert_eq!(json["error"], conflict);
    // ...and the refused edits left both monitors as they were.
    let (_, unchanged) = call(&app, "GET", &with_basic, Some(&member), None).await;
    assert_eq!(unchanged["url"], "https://example.com/");
    assert_eq!(unchanged["auth_username"], "monitor");
    let (_, unchanged) = call(&app, "GET", &with_url, Some(&member), None).await;
    assert_eq!(unchanged["auth_username"], Value::Null);

    // One request can swap one for the other.
    let (status, swapped) = call(
        &app,
        "PATCH",
        &with_basic,
        Some(&member),
        Some(json!({ "url": "https://user:url-secret-9d4e@example.com/", "auth_username": null })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{swapped}");
    assert_eq!(swapped["auth_username"], Value::Null);
    assert_eq!(swapped["has_auth_password"], false);
}

/// Editing how a monitor is requested makes it due now; keeping everything as
/// it is does not. A secret cannot be compared with its ciphertext, so
/// replacing one always counts.
#[sqlx::test]
async fn changing_request_config_reschedules_the_check_and_keeping_it_does_not(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(
        &app,
        "POST",
        &format!("/projects/{project}/monitors"),
        Some(&member),
        Some(configured_monitor()),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    let item = format!("/projects/{project}/monitors/{id}");

    for (label, body, reschedules) in [
        ("rename", json!({ "name": "other" }), false),
        (
            "same flags",
            json!({ "follow_redirects": false, "verify_ssl": false }),
            false,
        ),
        ("same method", json!({ "http_method": "POST" }), false),
        ("flag", json!({ "verify_ssl": true }), true),
        ("redirects", json!({ "follow_redirects": true }), true),
        ("body", json!({ "request_body": "{}" }), true),
        ("username", json!({ "auth_username": "other" }), true),
        ("password", json!({ "auth_password": "new" }), true),
        (
            "headers",
            json!({ "request_headers": [{ "name": "X-A", "value": "1" }] }),
            true,
        ),
        ("clear headers", json!({ "request_headers": null }), true),
        ("method", json!({ "http_method": "PUT" }), true),
    ] {
        make_not_due(&pool, &id).await;
        let (status, patched) = call(&app, "PATCH", &item, Some(&member), Some(body)).await;
        assert_eq!(status, StatusCode::OK, "{label}: {patched}");
        assert_eq!(is_due(&pool, &id).await, reschedules, "{label}");
    }
}

#[sqlx::test]
async fn switching_to_a_non_http_type_drops_the_request_config(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(
        &app,
        "POST",
        &format!("/projects/{project}/monitors"),
        Some(&member),
        Some(configured_monitor()),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();

    let (status, tcp) = call(
        &app,
        "PATCH",
        &format!("/projects/{project}/monitors/{id}"),
        Some(&member),
        Some(json!({ "check_type": "tcp", "host": "db.internal", "port": 5432 })),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "{tcp}");
    assert_eq!(tcp["http_method"], "GET");
    assert_eq!(tcp["request_body"], Value::Null);
    assert_eq!(tcp["auth_username"], Value::Null);
    assert_eq!(tcp["has_auth_password"], false);
    assert_eq!(tcp["request_headers"], json!([]));
    assert_eq!(tcp["follow_redirects"], true);
    assert_eq!(tcp["verify_ssl"], true);
    assert_eq!(stored_secrets(&pool, &id).await, (None, Vec::new(), None));

    // Secrets sent for a tcp monitor are dropped, never stored.
    let (status, tcp) = call(
        &app,
        "POST",
        &format!("/projects/{project}/monitors"),
        Some(&member),
        Some(json!({
            "name": "db", "check_type": "tcp", "host": "db.internal", "port": 5432,
            "request_headers": [{ "name": "X-Api-Key", "value": HEADER_SECRET }],
            "auth_username": "monitor", "auth_password": PASSWORD_SECRET,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{tcp}");
    assert_redacted(&tcp);
    assert_eq!(
        stored_secrets(&pool, tcp["id"].as_str().unwrap()).await,
        (None, Vec::new(), None)
    );
}

/// Viewers read monitors with the URL masked (Phase 1) and, now, without the
/// request body and the username either: both carry credentials as often as a
/// URL does. Nobody, member or admin, ever reads a header value or the
/// password. (Credentials in the URL and basic auth exclude each other, so the
/// two are read on two monitors.)
#[sqlx::test]
async fn viewers_get_masked_urls_and_no_secrets_and_members_still_no_secret_values(
    pool: sqlx::PgPool,
) {
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
    let basic_url = "https://status.example.com/basic";
    let body = format!("{{\"api_key\":\"{BODY_SECRET}\"}}");

    let (status, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(json!({
            "name": "API", "check_type": "http", "url": url, "http_method": "POST",
            "request_body": body,
            "request_headers": [{ "name": "X-Api-Key", "value": HEADER_SECRET }],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["url"], url, "the member who saved it sees it whole");
    let item = format!("{collection}/{}", created["id"].as_str().unwrap());
    let (status, created_basic) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(json!({
            "name": "Basic", "check_type": "http", "url": basic_url,
            "auth_username": "service-account", "auth_password": PASSWORD_SECRET,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created_basic}");
    let basic_item = format!("{collection}/{}", created_basic["id"].as_str().unwrap());

    let (status, detail) = call(&app, "GET", &item, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, basic_detail) = call(&app, "GET", &basic_item, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    let (_, list) = call(&app, "GET", &collection, Some(&viewer), None).await;
    let in_list = |name: &str| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|monitor| monitor["name"] == name)
            .unwrap_or_else(|| panic!("{name} is not in {list}"))
            .clone()
    };
    for shown in [&detail, &in_list("API")] {
        assert_eq!(shown["url"], redacted, "the masked URL composes");
        assert_eq!(shown["request_body"], "***", "set, but not readable");
        assert_eq!(shown["auth_username"], Value::Null, "none was set");
        assert_eq!(shown["has_auth_password"], false);
        assert_eq!(
            shown["request_headers"],
            json!([{ "name": "X-Api-Key", "value_set": true }])
        );
        assert_eq!(shown["http_method"], "POST");
        assert_eq!(shown["name"], "API", "everything else is as saved");
    }
    for shown in [&basic_detail, &in_list("Basic")] {
        assert_eq!(shown["url"], basic_url, "nothing in it to mask");
        assert_eq!(shown["auth_username"], "***");
        assert_eq!(shown["has_auth_password"], true);
        assert_eq!(shown["request_body"], Value::Null, "none was set");
        assert_eq!(shown["name"], "Basic", "everything else is as saved");
    }
    for body in [&detail, &basic_detail, &list] {
        assert_redacted(body);
        let text = body.to_string();
        for secret in [
            "s3cret-pw",
            "abc123",
            "frag-secret",
            "=eu",
            BODY_SECRET,
            "service-account",
        ] {
            assert!(!text.contains(secret), "{secret} reached a viewer: {text}");
        }
    }

    for (who, cookie) in [
        ("member", &member),
        ("project admin", &admin),
        ("global admin", &root),
    ] {
        let (_, detail) = call(&app, "GET", &item, Some(cookie), None).await;
        let (_, basic_detail) = call(&app, "GET", &basic_item, Some(cookie), None).await;
        let (_, list) = call(&app, "GET", &collection, Some(cookie), None).await;
        let in_list = |name: &str| {
            list.as_array()
                .unwrap()
                .iter()
                .find(|monitor| monitor["name"] == name)
                .unwrap()
                .clone()
        };
        for shown in [&detail, &in_list("API")] {
            assert_eq!(shown["url"], url, "{who}");
            assert_eq!(shown["request_body"], body, "{who}");
        }
        for shown in [&basic_detail, &in_list("Basic")] {
            assert_eq!(shown["auth_username"], "service-account", "{who}");
        }
        assert_redacted(&detail);
        assert_redacted(&basic_detail);
        assert_redacted(&list);
    }
}

/// The key is needed to write and to check, never to read: a lost or changed
/// `NAMI_ENCRYPTION_KEY` leaves every monitor listable and editable, and new
/// secrets are sealed under the key now in use.
#[sqlx::test]
async fn reads_and_edits_work_when_the_stored_secrets_no_longer_open(pool: sqlx::PgPool) {
    let first = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&first, &pool, project, Some("member")).await;
    let (_, created) = call(
        &first,
        "POST",
        &format!("/projects/{project}/monitors"),
        Some(&member),
        Some(configured_monitor()),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    let item = format!("/projects/{project}/monitors/{id}");

    let new_key = [42u8; 32];
    let second = app(AppState {
        cipher: Cipher::new(&new_key),
        ..AppState::for_test(pool.clone())
    });
    let (_, _, member) = user_with_role(&second, &pool, project, Some("member")).await;
    let (status, list) = call(
        &second,
        "GET",
        &format!("/projects/{project}/monitors"),
        Some(&member),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list[0]["has_auth_password"], true);
    let (status, renamed) = call(
        &second,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "name": "still editable" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{renamed}");
    let (stale, _, _) = stored_secrets(&pool, &id).await;
    let uuid = Uuid::parse_str(&id).unwrap();
    assert!(
        open_headers(&Cipher::new(&new_key), uuid, &stale.unwrap()).is_err(),
        "the old blob does not open under the new key"
    );

    let (status, replaced) = call(
        &second,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "auth_password": "sealed-anew" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{replaced}");
    let (_, _, password_enc) = stored_secrets(&pool, &id).await;
    assert_eq!(
        open_password(&Cipher::new(&new_key), uuid, &password_enc.unwrap())
            .unwrap()
            .expose(),
        "sealed-anew"
    );
}

/// No secret reaches the log, whatever the request does: created, edited,
/// read, listed or refused; statements are logged too (TRACE).
#[sqlx::test]
async fn secrets_never_reach_the_log(pool: sqlx::PgPool) {
    let log = LogCapture::default();
    let _logging = log.install_trace();
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/monitors");

    let mut with_body_secret = configured_monitor();
    with_body_secret["request_body"] = json!(BODY_SECRET);
    let (status, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(with_body_secret),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let item = format!("{collection}/{}", created["id"].as_str().unwrap());
    call(&app, "GET", &item, Some(&member), None).await;
    call(&app, "GET", &collection, Some(&member), None).await;
    call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "auth_password": "rotated-secret-5e1c", "request_headers": [{ "name": "X-New", "value": "new-header-secret-9a0d" }] })),
    )
    .await;
    let (status, _) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(json!({
            "name": "bad", "check_type": "http", "url": "https://example.com",
            "request_headers": [{ "name": "X-Api-Key", "value": format!("{LEAKY}\r\nX: 1") }],
            "auth_username": "a", "auth_password": format!("{LEAKY}\u{0}"),
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let lines = log.lines();
    assert!(
        lines
            .iter()
            .any(|line| line.contains("insert into monitors")),
        "the control: statements are in the log: {lines:?}"
    );
    for secret in [
        HEADER_SECRET,
        PASSWORD_SECRET,
        BODY_SECRET,
        LEAKY,
        "rotated-secret-5e1c",
        "new-header-secret-9a0d",
    ] {
        assert!(
            !lines.iter().any(|line| line.contains(secret)),
            "{secret} reached the log"
        );
    }
}

/// A request that carries secrets, and the same request seen as a monitor row:
/// nothing that holds one prints it, whichever way it is formatted.
#[test]
fn request_types_never_print_secrets() {
    let create: CreateMonitorRequest = serde_json::from_value(configured_monitor()).unwrap();
    let create_debug = format!("{create:?}");
    let config = create.into_config();
    let patch: PatchMonitorRequest = serde_json::from_value(json!({
        "request_headers": [{ "name": "X-Api-Key", "value": HEADER_SECRET }],
        "auth_password": PASSWORD_SECRET,
    }))
    .unwrap();
    let headers: Vec<RequestHeader> =
        serde_json::from_value(json!([{ "name": "X-Api-Key", "value": HEADER_SECRET }])).unwrap();
    // The sealed columns of a monitor row are not printed either.
    let monitor = Monitor {
        name: "Orders API".to_string(),
        request_headers_enc: Some(vec![0xDE, 0xAD, 0xBE, 0xEF]),
        request_header_names: vec!["X-Api-Key".to_string()],
        auth_password_enc: Some(vec![0xCA, 0xFE, 0xBA, 0xBE]),
        ..Monitor::default()
    };
    let view = MonitorView::new(monitor.clone(), false);

    let printed = [
        ("headers", format!("{headers:?}")),
        ("headers (pretty)", format!("{headers:#?}")),
        ("create", create_debug),
        ("patch", format!("{patch:?}")),
        ("patch (pretty)", format!("{patch:#?}")),
        ("config", format!("{config:?}")),
        ("headers change", format!("{:?}", config.request_headers)),
        ("password change", format!("{:?}", config.auth_password)),
        ("monitor", format!("{monitor:?}")),
        ("monitor (pretty)", format!("{monitor:#?}")),
        ("view", format!("{view:?}")),
    ];
    for (what, text) in &printed {
        for secret in [HEADER_SECRET, PASSWORD_SECRET, "222, 173", "202, 254"] {
            assert!(!text.contains(secret), "{what} printed {secret}: {text}");
        }
    }
    // Redaction is not blanking: the names and the rest stay visible.
    let shown = |what: &str| {
        printed
            .iter()
            .find(|(label, _)| *label == what)
            .unwrap()
            .1
            .clone()
    };
    assert!(shown("headers").contains("X-Api-Key"));
    assert!(shown("config").contains("Orders API"));
    assert!(shown("monitor").contains("<sealed>") && shown("monitor").contains("Orders API"));
    assert!(shown("view").contains("<sealed>"));
}

/// A client that sends the server's own columns (or what a response calls
/// them) gets nothing stored under them: only `request_headers` and
/// `auth_password` write secrets, and they seal them.
#[sqlx::test]
async fn the_sealed_columns_cannot_be_written_through_the_api(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/monitors");
    let forged = json!({
        "request_header_names": ["X-Forged"],
        "request_headers_enc": [1, 2, 3],
        "auth_password_enc": [4, 5, 6],
        "has_auth_password": true,
        "value_set": true,
    });
    let mut body = json!({ "name": "forged", "check_type": "http", "url": "https://example.com" });
    body.as_object_mut()
        .unwrap()
        .extend(forged.as_object().unwrap().clone());

    let (status, created) = call(&app, "POST", &collection, Some(&member), Some(body)).await;

    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["request_headers"], json!([]));
    assert_eq!(created["has_auth_password"], false);
    let id = created["id"].as_str().unwrap();
    assert_eq!(stored_secrets(&pool, id).await, (None, Vec::new(), None));

    let (status, patched) = call(
        &app,
        "PATCH",
        &format!("{collection}/{id}"),
        Some(&member),
        Some(forged),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["request_headers"], json!([]));
    assert_eq!(stored_secrets(&pool, id).await, (None, Vec::new(), None));
}

/// Only members write secrets, and not even a no-op patch gets a viewer or an
/// outsider anywhere near them: 403, nothing stored, nothing changed.
#[sqlx::test]
async fn viewers_and_outsiders_cannot_set_or_clear_secrets(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let collection = format!("/projects/{project}/monitors");
    let (_, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(configured_monitor()),
    )
    .await;
    let id = created["id"].as_str().unwrap().to_string();
    let item = format!("{collection}/{id}");
    let before = stored_secrets(&pool, &id).await;

    for (who, cookie) in [("viewer", &viewer), ("outsider", &outsider)] {
        let (status, _) = call(
            &app,
            "POST",
            &collection,
            Some(cookie),
            Some(configured_monitor()),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{who} creates");
        for body in [
            json!({ "request_headers": null, "auth_password": null }),
            json!({ "auth_password": "taken-over" }),
            json!({ "request_headers": [{ "name": "X-Evil", "value": "v" }] }),
        ] {
            let (status, _) = call(&app, "PATCH", &item, Some(cookie), Some(body.clone())).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{who} patches {body}");
        }
    }
    assert_eq!(stored_secrets(&pool, &id).await, before);
    let count: i64 = sqlx::query_scalar("select count(*) from monitors")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

/// Concurrent edits of different secrets and fields are all applied: a PATCH
/// that leaves a secret out neither loses nor resurrects one.
#[sqlx::test]
async fn concurrent_patches_of_secrets_and_other_fields_are_all_applied(pool: sqlx::PgPool) {
    const ROUNDS: usize = 8;
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let cipher = Cipher::new(&TEST_KEY);

    for round in 0..ROUNDS {
        let (_, created) = call(
            &app,
            "POST",
            &format!("/projects/{project}/monitors"),
            Some(&member),
            Some(configured_monitor()),
        )
        .await;
        let id = created["id"].as_str().unwrap().to_string();
        let uri = format!("/projects/{project}/monitors/{id}");
        let patches = [
            json!({ "request_headers": [{ "name": "X-New", "value": "header-v2" }] }),
            json!({ "auth_password": "password-v2" }),
            json!({ "name": "renamed" }),
            json!({ "request_body": "{\"v\":2}" }),
            json!({ "verify_ssl": true }),
        ];

        let mut requests = tokio::task::JoinSet::new();
        for body in patches {
            let (app, uri, member) = (app.clone(), uri.clone(), member.clone());
            requests
                .spawn(async move { call(&app, "PATCH", &uri, Some(&member), Some(body)).await });
        }
        while let Some(finished) = requests.join_next().await {
            let (status, body) = finished.unwrap();
            assert_eq!(status, StatusCode::OK, "round {round}: {body}");
        }

        let (_, current) = call(&app, "GET", &uri, Some(&member), None).await;
        assert_eq!(current["name"], "renamed", "round {round}");
        assert_eq!(current["request_body"], "{\"v\":2}", "round {round}");
        assert_eq!(current["verify_ssl"], true, "round {round}");
        assert_eq!(
            current["request_headers"],
            json!([{ "name": "X-New", "value_set": true }]),
            "round {round}"
        );
        let (headers_enc, names, password_enc) = stored_secrets(&pool, &id).await;
        let uuid = Uuid::parse_str(&id).unwrap();
        assert_eq!(names, vec!["X-New"], "round {round}");
        assert_eq!(
            open_headers(&cipher, uuid, &headers_enc.unwrap()).unwrap()[0]
                .value
                .expose(),
            "header-v2",
            "round {round}"
        );
        assert_eq!(
            open_password(&cipher, uuid, &password_enc.unwrap())
                .unwrap()
                .expose(),
            "password-v2",
            "round {round}"
        );
    }
}

/// A secret written as something other than a string (a numeric password, say)
/// is refused, and neither the error nor the log quotes it: serde's own
/// message would (`invalid type: integer `123456789`, expected a string`).
#[test]
fn a_secret_of_the_wrong_type_is_refused_without_quoting_it() {
    for (json, said) in [
        ("123456789", "number"),
        ("-42424242", "number"),
        ("1.5e300", "number"),
        ("18446744073709551615", "number"),
        ("true", "boolean"),
        ("null", "null"),
        ("[123456789]", "sequence"),
        ("{\"k\": \"map-secret\"}", "map"),
    ] {
        let error = serde_json::from_str::<SecretString>(json)
            .unwrap_err()
            .to_string();
        assert!(error.contains(said), "{json}: {error}");
        assert!(
            error.contains("expected a string"),
            "{json}: the message says what is wanted: {error}"
        );
        for digits in [
            "123456789",
            "42424242",
            "1.5",
            "18446744073709551615",
            "map-secret",
            "true",
        ] {
            assert!(
                !error.contains(digits),
                "{json}: {digits} quoted in {error}"
            );
        }
    }
    // Strings are taken as they are, from text and from a parsed value alike.
    for text in [
        "",
        "p@ss w\u{f6}rd",
        "line\nbreak",
        "\\\"quoted\"",
        "123456789",
    ] {
        let secret: SecretString = serde_json::from_value(json!(text)).unwrap();
        assert_eq!(secret.expose(), text);
        let secret: SecretString = serde_json::from_str(&json!(text).to_string()).unwrap();
        assert_eq!(secret.expose(), text);
    }
}

#[sqlx::test]
async fn a_secret_of_the_wrong_type_is_a_422_that_never_reaches_the_log(pool: sqlx::PgPool) {
    let log = LogCapture::default();
    let _logging = log.install_trace();
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/monitors");
    let (_, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(configured_monitor()),
    )
    .await;
    let item = format!("{collection}/{}", created["id"].as_str().unwrap());
    let before = stored_secrets(&pool, created["id"].as_str().unwrap()).await;

    let base = json!({ "name": "x", "check_type": "http", "url": "https://example.com" });
    let with = |extra: Value| {
        let mut body = base.clone();
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };
    for body in [
        json!({ "auth_username": "u", "auth_password": 123456789 }),
        json!({ "auth_username": "u", "auth_password": 4.25e200 }),
        json!({ "request_headers": [{ "name": "X-A", "value": 987654321 }] }),
        json!({ "request_headers": [{ "name": "X-A", "value": true }] }),
    ] {
        let (status, _) = call(
            &app,
            "POST",
            &collection,
            Some(&member),
            Some(with(body.clone())),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "POST {body}");
        let (status, _) = call(&app, "PATCH", &item, Some(&member), Some(body.clone())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "PATCH {body}");
    }

    let lines = log.lines();
    assert!(
        lines.iter().any(|line| line.contains("axum::rejection")),
        "the control: rejections are in the log: {lines:?}"
    );
    for number in ["123456789", "987654321", "4.25e200", "4.25e+200"] {
        assert!(
            !lines.iter().any(|line| line.contains(number)),
            "{number} reached the log"
        );
    }
    assert_eq!(
        stored_secrets(&pool, created["id"].as_str().unwrap()).await,
        before,
        "a refused edit stores nothing"
    );
}

/// The migration, seen without the API: the constraint lists the seven
/// methods (and still refuses everything else, for rows written around the
/// API), and a row that names no request config gets the safe defaults.
#[sqlx::test]
async fn the_database_allows_the_seven_methods_and_defaults_the_new_columns(pool: sqlx::PgPool) {
    let project = seed_project(&pool).await;
    for method in ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
        sqlx::query(
            "insert into monitors (project_id, name, check_type, url, http_method)
             values ($1, $2, 'http', 'https://example.com', $2)",
        )
        .bind(project)
        .bind(method)
        .execute(&pool)
        .await
        .unwrap_or_else(|e| panic!("{method} must be allowed: {e}"));
    }
    for method in ["TRACE", "CONNECT", "get", ""] {
        let refused = sqlx::query(
            "insert into monitors (project_id, name, check_type, url, http_method)
             values ($1, 'x', 'http', 'https://example.com', $2)",
        )
        .bind(project)
        .bind(method)
        .execute(&pool)
        .await
        .unwrap_err();
        let sqlx::Error::Database(db) = &refused else {
            panic!("{method:?}: {refused}");
        };
        assert_eq!(
            db.constraint(),
            Some("monitors_http_method_check"),
            "{method:?}"
        );
    }

    let id = seed_http_monitor(&pool, project, "plain", "https://example.com", 2).await;
    #[allow(clippy::type_complexity)]
    let row: (
        bool,
        bool,
        Vec<String>,
        Option<String>,
        Option<Vec<u8>>,
        Option<String>,
        Option<Vec<u8>>,
    ) = sqlx::query_as(
        "select follow_redirects, verify_ssl, request_header_names, request_body,
                request_headers_enc, auth_username, auth_password_enc
         from monitors where id = $1",
    )
    .bind(id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, (true, true, Vec::new(), None, None, None, None));
}

/// A global admin passes the membership check, so the project itself may not
/// exist: the insert's foreign key says so, and that is a 404 (not a 500)
/// with nothing stored, secrets included.
#[sqlx::test]
async fn creating_in_a_project_that_does_not_exist_is_a_404_for_a_global_admin(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let root_email = unique_email("root");
    seed_user(&pool, &root_email, "admin").await;
    let root = login(&app, &root_email).await;

    let (status, body) = call(
        &app,
        "POST",
        &format!("/projects/{}/monitors", Uuid::new_v4()),
        Some(&root),
        Some(configured_monitor()),
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    let count: i64 = sqlx::query_scalar("select count(*) from monitors")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

/// A udp payload carries SNMP communities and RADIUS secrets as often as a
/// body carries a token: viewers read `***`, and every view says whether it
/// was redacted, so a client never has to guess from the text.
#[sqlx::test]
async fn viewers_read_the_udp_payload_masked_and_views_say_they_were_redacted(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let collection = format!("/projects/{project}/monitors");
    let (status, created) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(json!({
            "name": "SNMP",
            "check_type": "udp",
            "host": "127.0.0.1",
            "port": 161,
            "udp_payload": "community=SNMP-COMMUNITY-SECRET",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["redacted"], false);
    let item = format!("{collection}/{}", created["id"].as_str().unwrap());

    let (_, list) = call(&app, "GET", &collection, Some(&viewer), None).await;
    let (_, detail) = call(&app, "GET", &item, Some(&viewer), None).await;
    for read in [&list[0], &detail] {
        assert_eq!(read["udp_payload"], "***", "{read}");
        assert_eq!(read["redacted"], true);
        assert!(!read.to_string().contains("SNMP-COMMUNITY-SECRET"));
    }
    let (_, list) = call(&app, "GET", &collection, Some(&member), None).await;
    let (_, detail) = call(&app, "GET", &item, Some(&member), None).await;
    for read in [&list[0], &detail] {
        assert_eq!(read["udp_payload"], "community=SNMP-COMMUNITY-SECRET");
        assert_eq!(read["redacted"], false);
    }
}

#[sqlx::test]
async fn blank_and_repeated_header_names_are_refused_through_the_api(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let collection = format!("/projects/{project}/monitors");
    for (headers, error) in [
        (
            json!([{ "name": "X-A", "value": "" }]),
            "header X-A: enter a value",
        ),
        (
            json!([{ "name": "X-A", "value": "1" }, { "name": "x-a", "value": "2" }]),
            "header x-a is listed twice",
        ),
        (
            json!([{ "name": "Upgrade", "value": "h2c" }]),
            "header Upgrade cannot be set: the checker manages Host, Content-Length, Transfer-Encoding, Connection and the other hop-by-hop headers",
        ),
    ] {
        let (status, body) = call(
            &app,
            "POST",
            &collection,
            Some(&member),
            Some(json!({
                "name": "API",
                "check_type": "http",
                "url": "https://example.com/",
                "request_headers": headers,
            })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{headers}");
        assert_eq!(body["error"], error);
    }
}
