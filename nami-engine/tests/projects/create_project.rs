// Creating a project through the API.
use crate::common;

use axum::http::StatusCode;
use common::{call, login, seed_user, test_app, unique_email};
use nami_engine::core::validate::MAX_LABEL_CHARS;
use serde_json::json;
use uuid::Uuid;

/// A project's name is a label: at most [`MAX_LABEL_CHARS`] chars, one line,
/// no control characters, stored trimmed. Unknown fields are refused.
#[sqlx::test]
async fn a_project_name_is_a_label_of_at_most_its_char_limit(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let email = unique_email("admin");
    seed_user(&pool, &email, "admin").await;
    let admin = login(&app, &email).await;
    let create = |name: String| {
        let (app, admin) = (app.clone(), admin.clone());
        async move {
            let slug = format!("p-{}", Uuid::new_v4().simple());
            call(&app, "POST", "/projects", Some(&admin), Some(json!({"name": name, "slug": slug}))).await
        }
    };

    let at = "é".repeat(MAX_LABEL_CHARS);
    let (status, body) = create(format!("  {at}  ")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["name"], json!(at), "stored trimmed");

    let (status, body) = create("é".repeat(MAX_LABEL_CHARS + 1)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"], format!("name must be at most {MAX_LABEL_CHARS} characters"));

    for name in ["api\nteam", "api\u{7}team", "api\u{202E}team", "   "] {
        let (status, body) = create(name.to_string()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{name:?}: {body}");
    }

    let (status, _) = call(&app, "POST", "/projects", Some(&admin),
        Some(json!({"name": "api", "slug": "api-extra", "owner": "someone"}))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "unknown fields are refused");
}
