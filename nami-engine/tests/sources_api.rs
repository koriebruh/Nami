// Log sources and their ingest tokens. Requires `docker compose up -d postgres`.
mod common;

use axum::http::StatusCode;
use common::{
    call, login, logs::unreachable_pool, seed_project, seed_user, test_app, unique_email,
    user_with_role,
};
use nami_engine::{
    auth::token::hash_token,
    core::crypto::{Cipher, TEST_KEY},
    logs::source::model::{self as source, TOKEN_CONTEXT},
};
use serde_json::{Value, json};
use uuid::Uuid;

/// The cipher `AppState::for_test` builds, so a test can open what the app
/// sealed (and seal what the app must refuse).
fn test_cipher() -> Cipher {
    Cipher::new(&TEST_KEY)
}

async fn create(
    app: &axum::Router,
    project: Uuid,
    cookie: &axum::http::HeaderValue,
    body: Value,
) -> Value {
    let (status, created) = call(
        app,
        "POST",
        &format!("/projects/{project}/sources"),
        Some(cookie),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    created
}

fn timestamp(value: &Value) -> chrono::DateTime<chrono::Utc> {
    value.as_str().unwrap().parse().unwrap()
}

fn id_of(source: &Value) -> Uuid {
    Uuid::parse_str(source["id"].as_str().unwrap()).unwrap()
}

/// No response may carry the stored token columns, under any name.
fn assert_no_stored_token_columns(body: &Value) {
    let text = body.to_string();
    assert!(
        !text.contains("token_hash") && !text.contains("token_enc"),
        "{text}"
    );
}

#[sqlx::test]
async fn members_create_sources_whose_token_is_stored_hashed_and_sealed(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;

    let created = create(&app, project, &member, json!({ "name": "  api  " })).await;
    assert_eq!(created["name"], "api");
    assert_eq!(created["platform"], "http");
    assert_eq!(created["retention_days"], 30);
    assert_eq!(created["daily_byte_quota"], Value::Null);
    assert_no_stored_token_columns(&created);
    let token = created["token"].as_str().unwrap().to_string();
    assert_eq!(
        token.len(),
        43,
        "32 random bytes, base64url without padding"
    );
    assert!(
        token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "{token}"
    );

    let source_id = id_of(&created);
    let (stored_hash, sealed): (String, Vec<u8>) =
        sqlx::query_as("select token_hash, token_enc from sources where id = $1")
            .bind(source_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored_hash, hash_token(&token));
    assert!(
        !sealed.windows(token.len()).any(|w| w == token.as_bytes()),
        "the token is not stored in the clear"
    );
    assert_eq!(
        test_cipher()
            .decrypt(&source::token_context(source_id), &sealed)
            .unwrap(),
        token.as_bytes()
    );
    assert!(
        test_cipher().decrypt(TOKEN_CONTEXT, &sealed).is_err(),
        "sealed for its row, not just for its column"
    );

    let full = create(
        &app,
        project,
        &member,
        json!({ "name": "edge", "platform": "syslog", "retention_days": 7, "daily_byte_quota": 1_048_576 }),
    )
    .await;
    assert_eq!(
        (full["platform"].clone(), full["retention_days"].clone()),
        (json!("syslog"), json!(7))
    );
    assert_eq!(full["daily_byte_quota"], 1_048_576);
}

#[sqlx::test]
async fn viewers_read_sources_but_never_their_tokens(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let created = create(&app, project, &member, json!({ "name": "api" })).await;
    let collection = format!("/projects/{project}/sources");
    let item = format!("{collection}/{}", created["id"].as_str().unwrap());

    let (status, list) = call(&app, "GET", &collection, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (status, one) = call(&app, "GET", &item, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    for body in [&list, &one] {
        assert!(
            !body.to_string().contains("token"),
            "viewers never see a token: {body}"
        );
    }

    assert_eq!(
        call(&app, "GET", &format!("{item}/token"), Some(&viewer), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &format!("{item}/rotate"),
            Some(&viewer),
            Some(json!({}))
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
            Some(json!({ "name": "x" }))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(
            &app,
            "POST",
            &collection,
            Some(&viewer),
            Some(json!({ "name": "x" }))
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

/// Security: a project's list holds that project's sources and no others,
/// in name order.
#[sqlx::test]
async fn the_list_holds_only_the_projects_own_sources_in_name_order(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, other_member) = user_with_role(&app, &pool, other, Some("member")).await;
    create(&app, project, &member, json!({ "name": "web" })).await;
    create(&app, project, &member, json!({ "name": "api" })).await;
    create(&app, other, &other_member, json!({ "name": "theirs" })).await;

    let (status, list) = call(
        &app,
        "GET",
        &format!("/projects/{project}/sources"),
        Some(&member),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|source| source["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["api", "web"]);
    for source in list.as_array().unwrap() {
        assert_eq!(source["project_id"], project.to_string());
    }
}

#[sqlx::test]
async fn members_reveal_and_rotate_and_a_rotated_token_stops_resolving_at_once(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let created = create(&app, project, &member, json!({ "name": "api" })).await;
    let old_token = created["token"].as_str().unwrap().to_string();
    let source_id = id_of(&created);
    let item = format!("/projects/{project}/sources/{source_id}");

    let (status, revealed) = call(&app, "GET", &format!("{item}/token"), Some(&member), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(revealed, json!({ "token": old_token }));
    assert_eq!(
        source::find_by_token(&pool, &old_token)
            .await
            .unwrap()
            .unwrap()
            .id,
        source_id
    );

    let (status, rotated) = call(
        &app,
        "POST",
        &format!("{item}/rotate"),
        Some(&member),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rotated}");
    assert_no_stored_token_columns(&rotated);
    let new_token = rotated["token"].as_str().unwrap().to_string();
    assert_ne!(new_token, old_token);
    assert!(timestamp(&rotated["updated_at"]) > timestamp(&created["updated_at"]));

    assert_eq!(
        source::find_by_token(&pool, &old_token).await.unwrap(),
        None,
        "the old token is dead at once"
    );
    let found = source::find_by_token(&pool, &new_token)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (found.id, found.project_id, found.retention_days),
        (source_id, project, 30)
    );
    let (_, revealed) = call(&app, "GET", &format!("{item}/token"), Some(&member), None).await;
    assert_eq!(revealed["token"], new_token.as_str());
}

#[sqlx::test]
async fn admins_delete_sources_and_a_deleted_source_token_resolves_nothing(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let created = create(&app, project, &member, json!({ "name": "api" })).await;
    let token = created["token"].as_str().unwrap().to_string();
    let item = format!(
        "/projects/{project}/sources/{}",
        created["id"].as_str().unwrap()
    );

    assert_eq!(
        call(&app, "DELETE", &item, Some(&member), None).await.0,
        StatusCode::FORBIDDEN
    );
    let (status, deleted) = call(&app, "DELETE", &item, Some(&admin), None).await;
    assert_eq!(
        (status, deleted),
        (StatusCode::OK, json!({ "deleted": true }))
    );
    assert_eq!(
        call(&app, "GET", &item, Some(&admin), None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "DELETE", &item, Some(&admin), None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(source::find_by_token(&pool, &token).await.unwrap(), None);
}

#[sqlx::test]
async fn outsiders_get_403_everywhere(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let created = create(&app, project, &member, json!({ "name": "api" })).await;
    let collection = format!("/projects/{project}/sources");
    let item = format!("{collection}/{}", created["id"].as_str().unwrap());

    for (method, uri, body) in [
        ("GET", collection.clone(), None),
        ("POST", collection.clone(), Some(json!({ "name": "x" }))),
        ("GET", item.clone(), None),
        ("PATCH", item.clone(), Some(json!({ "name": "x" }))),
        ("DELETE", item.clone(), None),
        ("GET", format!("{item}/token"), None),
        ("POST", format!("{item}/rotate"), Some(json!({}))),
    ] {
        assert_eq!(
            call(&app, method, &uri, Some(&outsider), body).await.0,
            StatusCode::FORBIDDEN,
            "{method} {uri}"
        );
    }
    assert_eq!(
        call(&app, "GET", &collection, None, None).await.0,
        StatusCode::UNAUTHORIZED
    );
}

/// Security: no route answers without a session, the token routes included.
#[sqlx::test]
async fn requests_without_a_session_get_401_everywhere(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let created = create(&app, project, &member, json!({ "name": "api" })).await;
    let collection = format!("/projects/{project}/sources");
    let item = format!("{collection}/{}", created["id"].as_str().unwrap());

    for (method, uri, body) in [
        ("GET", collection.clone(), None),
        ("POST", collection.clone(), Some(json!({ "name": "x" }))),
        ("GET", item.clone(), None),
        ("PATCH", item.clone(), Some(json!({ "name": "x" }))),
        ("DELETE", item.clone(), None),
        ("GET", format!("{item}/token"), None),
        ("POST", format!("{item}/rotate"), Some(json!({}))),
    ] {
        let (status, json) = call(&app, method, &uri, None, body).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {uri}");
        assert!(
            !json
                .to_string()
                .contains(created["token"].as_str().unwrap()),
            "{json}"
        );
    }
    let count: i64 = sqlx::query_scalar("select count(*) from sources")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "nothing was created or deleted");
}

#[sqlx::test]
async fn invalid_sources_are_rejected(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    for (body, fragment) in [
        (json!({ "name": " " }), "name is required"),
        (json!({ "name": "a\nb" }), "control characters"),
        (
            json!({ "name": "x", "platform": "fluentd" }),
            "platform must be one of http, syslog, vector, docker, other",
        ),
        (
            json!({ "name": "x", "retention_days": 0 }),
            "retention_days must be between 1 and 365",
        ),
        (
            json!({ "name": "x", "retention_days": 366 }),
            "retention_days must be between 1 and 365",
        ),
        (
            json!({ "name": "x", "daily_byte_quota": 1_048_575 }),
            "daily_byte_quota must be at least 1048576 bytes",
        ),
    ] {
        let (status, json) = call(
            &app,
            "POST",
            &format!("/projects/{project}/sources"),
            Some(&member),
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} returned {json}");
        assert!(
            json["error"].as_str().unwrap().contains(fragment),
            "{body}: {json}"
        );
    }
    let count: i64 = sqlx::query_scalar("select count(*) from sources")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn patch_merges_fields_and_null_clears_the_quota(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let created = create(
        &app,
        project,
        &member,
        json!({ "name": "api", "daily_byte_quota": 5_000_000 }),
    )
    .await;
    let item = format!(
        "/projects/{project}/sources/{}",
        created["id"].as_str().unwrap()
    );

    let (status, patched) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "retention_days": 90, "platform": "vector" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(
        (patched["name"].clone(), patched["retention_days"].clone()),
        (json!("api"), json!(90))
    );
    assert_eq!(patched["platform"], "vector");
    assert_eq!(
        patched["daily_byte_quota"], 5_000_000,
        "omitted fields are kept"
    );
    assert!(timestamp(&patched["updated_at"]) > timestamp(&created["updated_at"]));

    let (_, cleared) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "daily_byte_quota": null })),
    )
    .await;
    assert_eq!(cleared["daily_byte_quota"], Value::Null);
    let (status, json) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "retention_days": 0 })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
}

/// Security: a source of another project, addressed through a project the
/// caller can manage, is a 404 for every route and nothing changes.
#[sqlx::test]
async fn sources_of_another_project_are_404(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, _, other_member) = user_with_role(&app, &pool, other, Some("member")).await;
    let foreign = create(&app, other, &other_member, json!({ "name": "theirs" })).await;
    let item = format!(
        "/projects/{project}/sources/{}",
        foreign["id"].as_str().unwrap()
    );

    for (method, uri, body) in [
        ("GET", item.clone(), None),
        ("PATCH", item.clone(), Some(json!({ "name": "mine" }))),
        ("GET", format!("{item}/token"), None),
        ("POST", format!("{item}/rotate"), Some(json!({}))),
        ("DELETE", item.clone(), None),
    ] {
        assert_eq!(
            call(&app, method, &uri, Some(&admin), body).await.0,
            StatusCode::NOT_FOUND,
            "{method} {uri}"
        );
    }
    let token = foreign["token"].as_str().unwrap();
    let found = source::find_by_token(&pool, token).await.unwrap().unwrap();
    assert_eq!(
        found.project_id, other,
        "the foreign source and its token are untouched"
    );
    let name: String = sqlx::query_scalar("select name from sources where id = $1")
        .bind(found.id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "theirs");
}

#[sqlx::test]
async fn an_undecryptable_token_cannot_be_revealed_but_still_authenticates(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let created = create(&app, project, &member, json!({ "name": "api" })).await;
    let token = created["token"].as_str().unwrap();
    let source_id = id_of(&created);
    // Sealed under another key, as after a NAMI_ENCRYPTION_KEY change.
    let foreign =
        Cipher::new(&[9u8; 32]).encrypt(&source::token_context(source_id), token.as_bytes());
    sqlx::query("update sources set token_enc = $2 where id = $1")
        .bind(source_id)
        .bind(foreign)
        .execute(&pool)
        .await
        .unwrap();

    let (status, json) = call(
        &app,
        "GET",
        &format!("/projects/{project}/sources/{source_id}/token"),
        Some(&member),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert!(
        json["error"].as_str().unwrap().contains("rotate it"),
        "{json}"
    );
    assert_eq!(
        source::find_by_token(&pool, token)
            .await
            .unwrap()
            .unwrap()
            .id,
        source_id
    );
}

/// Security: a sealed token is bound to its row (associated data
/// `sources.token:{id}`), so one copied onto another source, as by someone
/// with write access to the table, opens nowhere but where it was sealed.
#[sqlx::test]
async fn a_sealed_token_copied_to_another_source_cannot_be_revealed(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let first = create(&app, project, &member, json!({ "name": "first" })).await;
    let second = create(&app, project, &member, json!({ "name": "second" })).await;
    let (first_id, second_id) = (id_of(&first), id_of(&second));
    let first_token = first["token"].as_str().unwrap();
    let second_token = second["token"].as_str().unwrap();
    sqlx::query(
        "update sources set token_enc = (select token_enc from sources where id = $1) where id = $2",
    )
    .bind(first_id)
    .bind(second_id)
    .execute(&pool)
    .await
    .unwrap();

    let reveal = |id: Uuid| format!("/projects/{project}/sources/{id}/token");
    let (status, json) = call(&app, "GET", &reveal(second_id), Some(&member), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{json}");
    assert!(
        !json.to_string().contains(first_token),
        "the other source's token is not shown: {json}"
    );
    let (status, json) = call(&app, "GET", &reveal(first_id), Some(&member), None).await;
    assert_eq!(
        (status, json),
        (StatusCode::OK, json!({ "token": first_token }))
    );
    assert_eq!(
        source::find_by_token(&pool, second_token)
            .await
            .unwrap()
            .unwrap()
            .id,
        second_id,
        "the copied value changes nothing about authentication"
    );

    // Rotating seals a fresh token for the row again.
    let (status, rotated) = call(
        &app,
        "POST",
        &format!("/projects/{project}/sources/{second_id}/rotate"),
        Some(&member),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rotated}");
    let (status, json) = call(&app, "GET", &reveal(second_id), Some(&member), None).await;
    assert_eq!(
        (status, json),
        (StatusCode::OK, json!({ "token": rotated["token"] }))
    );
}

/// A global admin needs no membership, so the project itself may not exist:
/// that is a 404, not a 500 from the foreign key.
#[sqlx::test]
async fn a_global_admin_creating_a_source_in_a_missing_project_gets_404(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let email = unique_email("global-admin");
    seed_user(&pool, &email, "admin").await;
    let admin = login(&app, &email).await;

    let (status, json) = call(
        &app,
        "POST",
        &format!("/projects/{}/sources", Uuid::new_v4()),
        Some(&admin),
        Some(json!({ "name": "api" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{json}");

    let project = seed_project(&pool).await;
    create(&app, project, &admin, json!({ "name": "api" })).await;
}

#[sqlx::test]
async fn find_by_token_ignores_empty_and_oversized_tokens(pool: sqlx::PgPool) {
    assert_eq!(source::find_by_token(&pool, "").await.unwrap(), None);
    assert_eq!(
        source::find_by_token(&pool, &"a".repeat(129))
            .await
            .unwrap(),
        None
    );
}

/// A token that cannot be a source's is never looked up: the answer is "no
/// such source" even where the database cannot answer at all, while a token
/// of the right shape does go to the database (and fails there).
#[tokio::test]
async fn find_by_token_never_queries_for_a_token_of_the_wrong_shape() {
    let unreachable = unreachable_pool();
    for token in [
        String::new(),
        "short".to_string(),
        "a".repeat(44),
        format!("{}+", "a".repeat(42)),
    ] {
        assert_eq!(
            source::find_by_token(&unreachable, &token).await.unwrap(),
            None,
            "{token:?}"
        );
    }
    assert!(
        source::find_by_token(&unreachable, &source::new_token())
            .await
            .is_err()
    );
}

/// A project has at most `MAX_SOURCES_PER_PROJECT` sources (each gets its own
/// share of the ingest limits); one more is 409, also when the requests race,
/// and deleting one makes room again.
#[sqlx::test]
async fn a_project_has_at_most_its_maximum_number_of_sources(pool: sqlx::PgPool) {
    use nami_engine::core::state::AppState;

    let mut state = AppState::for_test(pool.clone());
    state.config.max_sources_per_project = 3;
    let app = common::app(state);
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, _, other_member) = user_with_role(&app, &pool, other, Some("member")).await;

    // Racing creations cannot pass the cap together.
    let attempts: Vec<_> = (0..8)
        .map(|n| {
            let app = app.clone();
            let member = member.clone();
            tokio::spawn(async move {
                call(
                    &app,
                    "POST",
                    &format!("/projects/{project}/sources"),
                    Some(&member),
                    Some(json!({ "name": format!("s{n}") })),
                )
                .await
            })
        })
        .collect();
    let mut results = Vec::new();
    for attempt in attempts {
        results.push(attempt.await.unwrap());
    }
    let created: Vec<&Value> = results
        .iter()
        .filter(|(status, _)| *status == StatusCode::CREATED)
        .map(|(_, body)| body)
        .collect();
    assert_eq!(created.len(), 3, "{results:?}");
    for (status, body) in &results {
        if *status != StatusCode::CREATED {
            assert_eq!(*status, StatusCode::CONFLICT, "{body}");
            assert!(
                body["error"].as_str().unwrap().contains("at most 3"),
                "{body}"
            );
        }
    }
    let count: i64 = sqlx::query_scalar("select count(*) from sources where project_id = $1")
        .bind(project)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 3);

    // Another project has its own count.
    create(&app, other, &other_member, json!({ "name": "theirs" })).await;

    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/projects/{project}/sources/{}", id_of(created[0])),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    create(&app, project, &member, json!({ "name": "again" })).await;
}

/// Security: every response that carries a token tells caches not to keep
/// it (`Cache-Control: no-store`).
#[sqlx::test]
async fn token_responses_are_never_cached(pool: sqlx::PgPool) {
    use axum::http::{Request, header};
    use tower::ServiceExt;

    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let send = |method: &str, uri: String, body: Option<Value>| {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::COOKIE, member.clone());
        let body = match body {
            Some(json) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                axum::body::Body::from(json.to_string())
            }
            None => axum::body::Body::empty(),
        };
        let app = app.clone();
        let request = request.body(body).unwrap();
        async move { app.oneshot(request).await.unwrap() }
    };
    let created = send(
        "POST",
        format!("/projects/{project}/sources"),
        Some(json!({ "name": "api" })),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(created.headers()[header::CACHE_CONTROL], "no-store");
    let bytes = axum::body::to_bytes(created.into_body(), usize::MAX)
        .await
        .unwrap();
    let source_id = id_of(&serde_json::from_slice(&bytes).unwrap());
    for (method, path, body) in [("GET", "token", None), ("POST", "rotate", Some(json!({})))] {
        let response = send(
            method,
            format!("/projects/{project}/sources/{source_id}/{path}"),
            body,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            response.headers()[header::CACHE_CONTROL],
            "no-store",
            "{path}"
        );
    }
}
