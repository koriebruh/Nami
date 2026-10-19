// Monitor groups and monitors.group_id. Requires `docker compose up -d postgres`.
mod common;

use axum::http::StatusCode;
use common::{call, seed_http_monitor, seed_project, test_app, user_with_role};
use serde_json::{Value, json};
use uuid::Uuid;

async fn seed_group(pool: &sqlx::PgPool, project: Uuid, name: &str, position: i32) -> Uuid {
    sqlx::query_scalar(
        "insert into monitor_groups (project_id, name, position) values ($1, $2, $3) returning id",
    )
    .bind(project)
    .bind(name)
    .bind(position)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn group_of(pool: &sqlx::PgPool, monitor: Uuid) -> Option<Uuid> {
    sqlx::query_scalar("select group_id from monitors where id = $1")
        .bind(monitor)
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test]
async fn members_manage_groups_admins_delete_and_lists_are_ordered(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let collection = format!("/projects/{project}/monitor-groups");

    for (name, position) in [("Websites", 2), ("APIs", 1), ("Databases", 1)] {
        let (status, created) = call(
            &app,
            "POST",
            &collection,
            Some(&member),
            Some(json!({ "name": name, "position": position })),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{created}");
    }
    let (status, defaulted) = call(
        &app,
        "POST",
        &collection,
        Some(&member),
        Some(json!({ "name": "Misc" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{defaulted}");
    assert_eq!(defaulted["position"], 0);

    let (_, list) = call(&app, "GET", &collection, Some(&member), None).await;
    let names: Vec<&str> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["Misc", "APIs", "Databases", "Websites"],
        "position, then name"
    );

    let item = format!("{collection}/{}", defaulted["id"].as_str().unwrap());
    let (status, patched) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "name": "Other", "position": 9 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(
        (patched["name"].clone(), patched["position"].clone()),
        (json!("Other"), json!(9))
    );

    assert_eq!(
        call(&app, "DELETE", &item, Some(&member), None).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "DELETE", &item, Some(&admin), None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", &item, Some(&admin), None).await.0,
        StatusCode::NOT_FOUND
    );
}

#[sqlx::test]
async fn viewers_read_and_outsiders_get_403(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let group = seed_group(&pool, project, "APIs", 0).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let collection = format!("/projects/{project}/monitor-groups");
    let item = format!("{collection}/{group}");

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
            Some(json!({ "name": "x" }))
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
        call(&app, "GET", &collection, Some(&outsider), None)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "DELETE", &item, Some(&outsider), None).await.0,
        StatusCode::FORBIDDEN
    );
}

#[sqlx::test]
async fn invalid_groups_are_rejected(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    for (body, fragment) in [
        (json!({ "name": " " }), "name is required"),
        (json!({ "name": "a\nb" }), "control characters"),
        (
            json!({ "name": "x", "position": -1 }),
            "position must be between 0 and 1000000",
        ),
        (
            json!({ "name": "x", "position": 1_000_001 }),
            "position must be between 0 and 1000000",
        ),
    ] {
        let (status, json) = call(
            &app,
            "POST",
            &format!("/projects/{project}/monitor-groups"),
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
    let count: i64 = sqlx::query_scalar("select count(*) from monitor_groups")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
}

#[sqlx::test]
async fn monitors_join_and_leave_groups_of_their_own_project(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let group = seed_group(&pool, project, "APIs", 0).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let body = json!({ "name": "Orders", "check_type": "http", "url": "https://example.com", "group_id": group });

    let (status, created) = call(
        &app,
        "POST",
        &format!("/projects/{project}/monitors"),
        Some(&member),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["group_id"], group.to_string());
    let (_, list) = call(
        &app,
        "GET",
        &format!("/projects/{project}/monitors"),
        Some(&member),
        None,
    )
    .await;
    assert!(list.is_array(), "the list stays a flat array");
    assert_eq!(list[0]["group_id"], group.to_string());

    let item = format!(
        "/projects/{project}/monitors/{}",
        created["id"].as_str().unwrap()
    );
    sqlx::query("update monitors set next_check_at = now() + interval '1 hour'")
        .execute(&pool)
        .await
        .unwrap();
    let (status, ungrouped) = call(
        &app,
        "PATCH",
        &item,
        Some(&member),
        Some(json!({ "group_id": null })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ungrouped}");
    assert_eq!(ungrouped["group_id"], Value::Null);
    let due: bool = sqlx::query_scalar("select next_check_at <= now() from monitors")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!due, "regrouping is not a check change");
}

/// Security: a group of another project (or no group at all) is a 404 and
/// nothing changes; the database refuses the reference on its own too.
#[sqlx::test]
async fn groups_of_another_project_are_404(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let foreign_group = seed_group(&pool, other, "Theirs", 0).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;

    let foreign_item = format!("/projects/{project}/monitor-groups/{foreign_group}");
    assert_eq!(
        call(&app, "GET", &foreign_item, Some(&admin), None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(
            &app,
            "PATCH",
            &foreign_item,
            Some(&admin),
            Some(json!({ "name": "hijacked" }))
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&app, "DELETE", &foreign_item, Some(&admin), None)
            .await
            .0,
        StatusCode::NOT_FOUND
    );

    for group in [foreign_group, Uuid::new_v4()] {
        let body = json!({ "name": "x", "check_type": "http", "url": "https://example.com", "group_id": group });
        let (status, _) = call(
            &app,
            "POST",
            &format!("/projects/{project}/monitors"),
            Some(&admin),
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(
            &app,
            "PATCH",
            &format!("/projects/{project}/monitors/{monitor}"),
            Some(&admin),
            Some(json!({ "group_id": group })),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
    let count: i64 = sqlx::query_scalar("select count(*) from monitors")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1, "no monitor was created");
    assert_eq!(
        group_of(&pool, monitor).await,
        None,
        "the monitor was not moved"
    );
    let name: String = sqlx::query_scalar("select name from monitor_groups where id = $1")
        .bind(foreign_group)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(name, "Theirs");
    let direct = sqlx::query("update monitors set group_id = $1 where id = $2")
        .bind(foreign_group)
        .bind(monitor)
        .execute(&pool)
        .await;
    assert!(
        direct.is_err(),
        "the composite foreign key rejects a cross-project group"
    );
}

#[sqlx::test]
async fn deleting_a_group_ungroups_its_monitors(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let group = seed_group(&pool, project, "APIs", 0).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    sqlx::query("update monitors set group_id = $1 where id = $2")
        .bind(group)
        .bind(monitor)
        .execute(&pool)
        .await
        .unwrap();
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;

    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/projects/{project}/monitor-groups/{group}"),
        Some(&admin),
        None,
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        group_of(&pool, monitor).await,
        None,
        "the monitor stays, ungrouped"
    );
    let project_kept: Uuid = sqlx::query_scalar("select project_id from monitors where id = $1")
        .bind(monitor)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(project_kept, project, "only group_id was cleared");
}

#[sqlx::test]
async fn invalid_group_patches_are_rejected_and_change_nothing(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let group = seed_group(&pool, project, "APIs", 3).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let item = format!("/projects/{project}/monitor-groups/{group}");
    for body in [
        json!({ "name": "\u{202e}evil" }),
        json!({ "name": "" }),
        json!({ "position": -1 }),
        json!({ "position": 1_000_001 }),
    ] {
        let (status, json) = call(&app, "PATCH", &item, Some(&member), Some(body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {json}");
    }
    let (name, position): (String, i32) =
        sqlx::query_as("select name, position from monitor_groups where id = $1")
            .bind(group)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!((name.as_str(), position), ("APIs", 3));
}

#[sqlx::test]
async fn deleting_a_group_ungroups_its_heartbeats_and_monitors(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let group = seed_group(&pool, project, "Shared", 0).await;
    let monitor = seed_http_monitor(&pool, project, "API", "https://example.com", 2).await;
    sqlx::query("update monitors set group_id = $2 where id = $1")
        .bind(monitor)
        .bind(group)
        .execute(&pool)
        .await
        .unwrap();
    let (status, created) = call(
        &app,
        "POST",
        &format!("/projects/{project}/heartbeats"),
        Some(&member),
        Some(json!({ "name": "Backup", "period_seconds": 60, "group_id": group })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let heartbeat: Uuid = created["id"].as_str().unwrap().parse().unwrap();

    let (status, _) = call(
        &app,
        "DELETE",
        &format!("/projects/{project}/monitor-groups/{group}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(group_of(&pool, monitor).await, None);
    let kept: Option<Uuid> = sqlx::query_scalar("select group_id from heartbeats where id = $1")
        .bind(heartbeat)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kept, None, "the heartbeat survives, ungrouped");
}
