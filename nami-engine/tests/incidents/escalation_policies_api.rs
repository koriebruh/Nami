// Escalation policies. Requires `docker compose up -d postgres`.
use crate::common;

use axum::http::StatusCode;
use common::{call, insert_channel, seed_project, test_app, user_with_role};
use nami_engine::incidents::channel::model::ChannelConfig;
use nami_engine::incidents::escalation::model::policy_targeting_channel;
use serde_json::{Value, json};
use uuid::Uuid;

#[sqlx::test]
async fn a_project_starts_with_a_default_policy_paging_all_members(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (status, body) = call(&app, "GET", &format!("/projects/{project}/escalation-policies"), Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["name"], "Default");
    assert_eq!(body[0]["is_default"], true);
    assert_eq!(body[0]["steps"][0]["delay_minutes"], 0);
    assert_eq!(body[0]["steps"][0]["targets"][0], json!({"type": "all_members", "label": "All members"}));
}

#[sqlx::test]
async fn admins_build_multi_step_policies_with_conditions_and_a_fallback(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let room = insert_channel(&pool, Some(project), None, "ops", ChannelConfig::Slack { webhook_url: "https://hooks.slack.com/services/x".into() }).await;
    let path = format!("/projects/{project}/escalation-policies");
    let (_, list) = call(&app, "GET", &path, Some(&admin), None).await;
    let default_id = list[0]["id"].as_str().unwrap().to_string();
    let body = json!({
        "name": "Business hours", "repeat_count": 2, "fallback_policy_id": default_id,
        "steps": [
            {"delay_minutes": 0, "targets": [{"type": "user", "id": alice}, {"type": "project_channel", "id": room}],
             "condition": {"weekdays": [1,2,3,4,5], "from": "09:00", "to": "17:00", "timezone": "Asia/Jakarta"}},
            {"delay_minutes": 15, "targets": [{"type": "all_members"}]}
        ]});
    let (status, _) = call(&app, "POST", &path, Some(&member), Some(body.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, created) = call(&app, "POST", &path, Some(&admin), Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["steps"][0]["targets"][0]["label"], alice_email);
    assert_eq!(created["steps"][0]["targets"][1]["label"], "ops (slack)");
    assert_eq!(created["steps"][1]["delay_minutes"], 15);
    let id = created["id"].as_str().unwrap();

    let (status, _) = call(&app, "DELETE", &format!("/projects/{project}/notification-channels/{room}"), Some(&admin), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "a targeted channel cannot be deleted");
    assert_eq!(policy_targeting_channel(&pool, seed_project(&pool).await, room).await.unwrap(), None,
        "another project never learns the policy's name");
    let (status, _) = call(&app, "POST", &format!("{path}/{id}/default"), Some(&admin), Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&app, "DELETE", &format!("{path}/{id}"), Some(&admin), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "the default cannot be deleted");
    let (status, _) = call(&app, "DELETE", &format!("{path}/{default_id}"), Some(&admin), None).await;
    assert_eq!(status, StatusCode::OK, "the old default can go now");
}

#[sqlx::test]
async fn policies_are_validated_and_scoped_to_their_project(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (stranger, _, _) = user_with_role(&app, &pool, other, Some("member")).await;
    let foreign_room = insert_channel(&pool, Some(other), None, "x", ChannelConfig::Slack { webhook_url: "https://hooks.slack.com/services/y".into() }).await;
    let personal = insert_channel(&pool, None, Some(alice), "mine", ChannelConfig::Slack { webhook_url: "https://hooks.slack.com/services/z".into() }).await;
    let other_policy: Uuid = sqlx::query_scalar("select id from escalation_policies where project_id = $1").bind(other).fetch_one(&pool).await.unwrap();
    let path = format!("/projects/{project}/escalation-policies");
    let step = |targets: Value| json!([{"delay_minutes": 0, "targets": targets}]);
    let cases = [
        json!({"name": "a", "steps": []}),
        json!({"name": "a", "steps": step(json!([]))}),
        json!({"name": "a", "repeat_count": 11, "steps": step(json!([{"type": "all_members"}]))}),
        json!({"name": "a", "steps": [{"delay_minutes": 1441, "targets": [{"type": "all_members"}]}]}),
        json!({"name": "a", "steps": step(json!([{"type": "user", "id": stranger}]))}),
        json!({"name": "a", "steps": step(json!([{"type": "project_channel", "id": foreign_room}]))}),
        json!({"name": "a", "steps": step(json!([{"type": "project_channel", "id": personal}]))}),
        json!({"name": "a", "steps": step(json!([{"type": "all_members"}, {"type": "all_members"}]))}),
        json!({"name": "a", "fallback_policy_id": other_policy, "steps": step(json!([{"type": "all_members"}]))}),
        json!({"name": "a", "steps": [{"delay_minutes": 0, "targets": [{"type": "all_members"}],
               "condition": {"weekdays": [1], "from": "09:00", "to": "17:00", "timezone": "Nowhere/Land"}}]}),
    ];
    for body in cases {
        let (status, response) = call(&app, "POST", &path, Some(&admin), Some(body.clone())).await;
        assert!(status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY, "{body} -> {status} {response}");
    }
    let (status, created) = call(&app, "POST", &path, Some(&admin),
        Some(json!({"name": "self", "steps": step(json!([{"type": "all_members"}]))}))).await;
    assert_eq!(status, StatusCode::CREATED);
    let id = created["id"].as_str().unwrap();
    let (status, _) = call(&app, "PUT", &format!("{path}/{id}"), Some(&admin),
        Some(json!({"name": "self", "fallback_policy_id": id, "steps": step(json!([{"type": "all_members"}]))}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "no fallback to itself");
    let (_, _, other_admin) = user_with_role(&app, &pool, other, Some("admin")).await;
    let (status, _) = call(&app, "GET", &format!("/projects/{other}/escalation-policies/{id}"), Some(&other_admin), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn opening_an_incident_arms_escalation_on_its_policy(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/escalation-policies"), Some(&admin),
        Some(json!({"name": "slow", "steps": [{"delay_minutes": 7, "targets": [{"type": "all_members"}]}]}))).await;
    let (_, incident) = call(&app, "POST", &format!("/projects/{project}/incidents"), Some(&member),
        Some(json!({"title": "x", "escalation_policy_id": created["id"]}))).await;
    let (policy, step, next, started): (Uuid, i32, Option<chrono::DateTime<chrono::Utc>>, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("select escalation_policy_id, escalation_step, next_escalation_at, started_at from incidents where id = $1::uuid")
            .bind(incident["id"].as_str().unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(policy.to_string(), created["id"].as_str().unwrap());
    assert_eq!(step, 0);
    assert_eq!(next, Some(started + chrono::Duration::minutes(7)));

    let id = incident["id"].as_str().unwrap();
    call(&app, "POST", &format!("/projects/{project}/incidents/{id}/acknowledge"), Some(&member), Some(json!({}))).await;
    let next: Option<chrono::DateTime<chrono::Utc>> = sqlx::query_scalar("select next_escalation_at from incidents where id = $1::uuid")
        .bind(id).fetch_one(&pool).await.unwrap();
    assert_eq!(next, None, "acknowledge stops escalation");
}

#[sqlx::test]
async fn resolving_stops_escalation_and_reopening_restarts_it_in_a_new_generation(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let policies = format!("/projects/{project}/escalation-policies");
    let (_, slow) = call(&app, "POST", &policies, Some(&admin),
        Some(json!({"name": "slow", "steps": [{"delay_minutes": 7, "targets": [{"type": "all_members"}]}]}))).await;
    let slow_id: Uuid = slow["id"].as_str().unwrap().parse().unwrap();
    let (_, incident) = call(&app, "POST", &format!("/projects/{project}/incidents"), Some(&member),
        Some(json!({"title": "x", "escalation_policy_id": slow_id}))).await;
    let incident = format!("/projects/{project}/incidents/{}", incident["id"].as_str().unwrap());
    let cursor = || async {
        sqlx::query_as::<_, (Option<Uuid>, i32, Option<chrono::DateTime<chrono::Utc>>)>(
            "select escalation_policy_id, escalation_generation, next_escalation_at from incidents where project_id = $1")
            .bind(project).fetch_one(&pool).await.unwrap()
    };
    let reopen = || async {
        let (status, _) = call(&app, "POST", &format!("{incident}/resolve"), Some(&member), Some(json!({}))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(cursor().await.2, None, "resolve stops escalation");
        let before = chrono::Utc::now();
        let (status, _) = call(&app, "POST", &format!("{incident}/reopen"), Some(&member), Some(json!({}))).await;
        assert_eq!(status, StatusCode::OK);
        (before, chrono::Utc::now())
    };

    let (before, after) = reopen().await;
    let (policy, generation, next) = cursor().await;
    assert_eq!((policy, generation), (Some(slow_id), 1), "the incident keeps its own policy");
    let next = next.unwrap();
    assert!(next >= before + chrono::Duration::minutes(7) && next <= after + chrono::Duration::minutes(7));

    let (status, _) = call(&app, "DELETE", &format!("{policies}/{slow_id}"), Some(&admin), None).await;
    assert_eq!(status, StatusCode::OK);
    let (before, after) = reopen().await;
    let default: Uuid = sqlx::query_scalar("select id from escalation_policies where project_id = $1 and is_default")
        .bind(project).fetch_one(&pool).await.unwrap();
    let (policy, generation, next) = cursor().await;
    assert_eq!((policy, generation), (Some(default), 2), "a deleted policy falls back to the default");
    assert!((before..=after).contains(&next.unwrap()), "the default's first step has no delay");
}

#[sqlx::test]
async fn a_project_holds_at_most_the_policy_limit(pool: sqlx::PgPool) {
    use nami_engine::incidents::escalation::model::MAX_POLICIES_PER_PROJECT;
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    // With Default, one short of the limit.
    sqlx::query("insert into escalation_policies (project_id, name) select $1, 'p' || n from generate_series(1, $2) as n")
        .bind(project).bind(MAX_POLICIES_PER_PROJECT - 2).execute(&pool).await.unwrap();
    let path = format!("/projects/{project}/escalation-policies");
    let body = |name: &str| json!({"name": name, "steps": [{"delay_minutes": 0, "targets": [{"type": "all_members"}]}]});
    let (status, _) = call(&app, "POST", &path, Some(&admin), Some(body("last"))).await;
    assert_eq!(status, StatusCode::CREATED, "the limit itself is allowed");
    let (status, _) = call(&app, "POST", &path, Some(&admin), Some(body("one too many"))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(&app, "POST", &path, Some(&admin), Some(body("Default"))).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn only_admins_change_policies(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let path = format!("/projects/{project}/escalation-policies");
    let (_, list) = call(&app, "GET", &path, Some(&member), None).await;
    let id = list[0]["id"].as_str().unwrap();
    let body = json!({"name": "Default", "steps": [{"delay_minutes": 0, "targets": [{"type": "all_members"}]}]});
    for (method, uri, body) in [
        ("PUT", format!("{path}/{id}"), Some(body)),
        ("POST", format!("{path}/{id}/default"), Some(json!({}))),
        ("DELETE", format!("{path}/{id}"), None),
    ] {
        let (status, _) = call(&app, method, &uri, Some(&member), body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
    }
}

#[sqlx::test]
async fn a_manual_incident_cannot_use_another_projects_policy(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let foreign: Uuid = sqlx::query_scalar("select id from escalation_policies where project_id = $1")
        .bind(other).fetch_one(&pool).await.unwrap();
    let (status, _) = call(&app, "POST", &format!("/projects/{project}/incidents"), Some(&member),
        Some(json!({"title": "x", "escalation_policy_id": foreign}))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[sqlx::test]
async fn a_targeted_schedule_cannot_be_deleted(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (alice, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (status, schedule) = call(&app, "POST", &format!("/projects/{project}/schedules"), Some(&admin), Some(json!({
        "name": "primary", "timezone": "UTC", "rotation": "weekly", "handoff_time": "09:00",
        "start_date": chrono::Utc::now().date_naive(), "participants": [alice]}))).await;
    assert_eq!(status, StatusCode::CREATED, "{schedule}");
    let sid = schedule["id"].as_str().unwrap();
    let (status, _) = call(&app, "POST", &format!("/projects/{project}/escalation-policies"), Some(&admin),
        Some(json!({"name": "on call", "steps": [{"delay_minutes": 0, "targets": [{"type": "schedule", "id": sid}]}]}))).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, body) = call(&app, "DELETE", &format!("/projects/{project}/schedules/{sid}"), Some(&admin), None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["error"], "schedule is used by escalation policy \"on call\"");
}
