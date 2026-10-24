// On-call schedules. Requires `docker compose up -d postgres`.
use crate::common;

use axum::http::StatusCode;
use chrono::{DateTime, Duration, Months, NaiveDate, Utc};
use common::{call, seed_project, test_app, user_with_role};
use nami_engine::incidents::oncall::model::{
    self as oncall, EARLIEST_TIME, LATEST_TIME, MAX_OVERRIDE_DAYS, MAX_OVERRIDES_PER_SCHEDULE,
    MAX_SCHEDULES_PER_PROJECT, MAX_SHIFT_DAYS, MAX_START_DATE_YEARS,
};
use nami_engine::incidents::oncall::rotation::RotationLength;
use serde_json::{Value, json};
use uuid::Uuid;

fn schedule_body(participants: &[Uuid]) -> Value {
    json!({"name": "primary", "timezone": "Europe/Berlin", "rotation": "weekly", "handoff_time": "09:00",
           "start_date": (Utc::now() - Duration::days(1)).date_naive(), "participants": participants,
           "restrictions": [{"weekdays": [1,2,3,4,5,6,7], "from": "00:00", "to": "00:00"}]})
}

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

fn shifts_uri(base: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> String {
    format!("{base}/shifts?from={}&to={}", urlencode(&from.to_rfc3339()), urlencode(&to.to_rfc3339()))
}

#[sqlx::test]
async fn admins_manage_schedules_and_viewers_see_who_is_on_call(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let path = format!("/projects/{project}/schedules");

    let (status, _) = call(&app, "POST", &path, Some(&member), Some(schedule_body(&[alice]))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, created) = call(&app, "POST", &path, Some(&admin), Some(schedule_body(&[alice]))).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["participants"][0]["email"], alice_email);
    assert_eq!(created["on_call_now"]["user_id"], json!(alice));
    let id = created["id"].as_str().unwrap();

    let (status, shifts) = call(&app, "GET", &format!("{path}/{id}/shifts"), Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{shifts}");
    assert!(!shifts.as_array().unwrap().is_empty());
    let (status, board) = call(&app, "GET", &format!("/projects/{project}/on-call"), Some(&viewer), None).await;
    assert_eq!((status, board[0]["on_call_now"]["email"].as_str()), (StatusCode::OK, Some(alice_email.as_str())));
}

#[sqlx::test]
async fn viewers_list_project_members_and_outsiders_get_403(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (alice, alice_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let path = format!("/projects/{project}/members");
    let (status, members) = call(&app, "GET", &path, Some(&viewer), None).await;
    assert_eq!(status, StatusCode::OK, "{members}");
    let entry = members.as_array().unwrap().iter().find(|m| m["user_id"] == json!(alice)).unwrap().clone();
    assert_eq!(entry, json!({"user_id": alice, "email": alice_email, "project_role": "member"}));
    let (status, _) = call(&app, "GET", &path, Some(&outsider), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn the_project_tells_the_caller_its_effective_role(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    for role in ["admin", "member", "viewer"] {
        let (_, _, cookie) = user_with_role(&app, &pool, project, Some(role)).await;
        let (status, body) = call(&app, "GET", &format!("/projects/{project}"), Some(&cookie), None).await;
        assert_eq!((status, body["role"].as_str()), (StatusCode::OK, Some(role)), "{body}");
    }
    let email = common::unique_email("root");
    common::seed_user(&pool, &email, "admin").await;
    let root = common::login(&app, &email).await;
    let (_, body) = call(&app, "GET", &format!("/projects/{project}"), Some(&root), None).await;
    assert_eq!(body["role"], "admin", "a global admin without membership is admin");
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let (status, _) = call(&app, "GET", &format!("/projects/{project}"), Some(&outsider), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn schedules_are_validated(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (stranger, _, _) = user_with_role(&app, &pool, other, Some("member")).await;
    let path = format!("/projects/{project}/schedules");
    let mut cases = Vec::new();
    for (key, value) in [("timezone", json!("Mars/Olympus")), ("rotation", json!("monthly")),
                         ("handoff_time", json!("25:00")), ("participants", json!([])),
                         ("participants", json!([stranger])), ("participants", json!([alice, alice])),
                         ("restrictions", json!([{"weekdays": [9], "from": "09:00", "to": "17:00"}]))] {
        let mut body = schedule_body(&[alice]);
        body[key] = value;
        cases.push(body);
    }
    let mut custom = schedule_body(&[alice]);
    custom["rotation"] = json!("custom");
    cases.push(custom.clone());
    custom["rotation_hours"] = json!(0);
    cases.push(custom);
    for body in cases {
        let (status, response) = call(&app, "POST", &path, Some(&admin), Some(body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body} -> {response}");
    }
}

#[sqlx::test]
async fn overrides_put_someone_else_on_call_and_foreign_ids_are_404(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let other = seed_project(&pool).await;
    let (admin_id, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (bob, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, other_admin) = user_with_role(&app, &pool, other, Some("admin")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/schedules"), Some(&admin), Some(schedule_body(&[alice]))).await;
    let id = created["id"].as_str().unwrap().to_string();
    let overrides = format!("/projects/{project}/schedules/{id}/overrides");
    let window = json!({"user_id": bob, "starts_at": Utc::now() - Duration::minutes(1), "ends_at": Utc::now() + Duration::hours(2)});

    let (status, _) = call(&app, "POST", &overrides, Some(&member), Some(window.clone())).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "only admins manage overrides in Part 1");
    let (status, o) = call(&app, "POST", &overrides, Some(&admin), Some(window)).await;
    assert_eq!(status, StatusCode::CREATED, "{o}");
    assert_eq!(o["created_by"], json!(admin_id));
    let (_, now) = call(&app, "GET", &format!("/projects/{project}/schedules/{id}"), Some(&admin), None).await;
    assert_eq!(now["on_call_now"]["user_id"], json!(bob));

    for bad in [json!({"user_id": bob, "starts_at": Utc::now(), "ends_at": Utc::now() - Duration::hours(1)}),
                json!({"user_id": bob, "starts_at": Utc::now(), "ends_at": Utc::now() + Duration::days(91)}),
                json!({"user_id": Uuid::new_v4(), "starts_at": Utc::now(), "ends_at": Utc::now() + Duration::hours(1)})] {
        let (status, _) = call(&app, "POST", &overrides, Some(&admin), Some(bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, _) = call(&app, "GET", &format!("/projects/{other}/schedules/{id}"), Some(&other_admin), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, "DELETE", &format!("/projects/{other}/schedules/{id}/overrides/{}", o["id"].as_str().unwrap()),
        Some(&other_admin), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, listed) = call(&app, "GET", &overrides, Some(&admin), None).await;
    assert_eq!((status, listed[0]["id"].clone()), (StatusCode::OK, o["id"].clone()));
    let (status, _) = call(&app, "DELETE", &format!("{overrides}/{}", o["id"].as_str().unwrap()), Some(&admin), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, now) = call(&app, "GET", &format!("/projects/{project}/schedules/{id}"), Some(&admin), None).await;
    assert_eq!(now["on_call_now"]["user_id"], json!(alice), "the rotation is back");
}

#[sqlx::test]
async fn shift_ranges_are_capped(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/schedules"), Some(&admin), Some(schedule_body(&[alice]))).await;
    let base = format!("/projects/{project}/schedules/{}", created["id"].as_str().unwrap());
    let from = Utc::now();
    let max = Duration::days(MAX_SHIFT_DAYS);
    let cases = [
        (from, from + max, StatusCode::OK),
        (from, from + max + Duration::seconds(1), StatusCode::BAD_REQUEST),
        (from, from + Duration::days(32), StatusCode::BAD_REQUEST),
        (from, from, StatusCode::BAD_REQUEST),
        (EARLIEST_TIME, EARLIEST_TIME + Duration::days(1), StatusCode::OK),
        (EARLIEST_TIME - Duration::seconds(1), EARLIEST_TIME + Duration::days(1), StatusCode::BAD_REQUEST),
        (LATEST_TIME - Duration::days(1), LATEST_TIME, StatusCode::OK),
        (LATEST_TIME - Duration::days(1), LATEST_TIME + Duration::seconds(1), StatusCode::BAD_REQUEST),
    ];
    for (from, to, expected) in cases {
        let (status, body) = call(&app, "GET", &shifts_uri(&base, from, to), Some(&admin), None).await;
        assert_eq!(status, expected, "{from} .. {to}: {body}");
    }
    let (status, body) = call(&app, "GET", &format!("{base}/shifts?from=yesterday"), Some(&admin), None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a malformed time answers in the API's JSON shape");
    assert!(body["error"].is_string(), "{body}");
}

#[sqlx::test]
async fn override_times_are_bounded(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/schedules"), Some(&admin), Some(schedule_body(&[alice]))).await;
    let overrides = format!("/projects/{project}/schedules/{}/overrides", created["id"].as_str().unwrap());
    let start = Utc::now() + Duration::hours(1);
    let longest = Duration::days(MAX_OVERRIDE_DAYS);
    let cases = [
        (start, start + longest, StatusCode::CREATED),
        (start, start + longest + Duration::seconds(1), StatusCode::BAD_REQUEST),
        (LATEST_TIME - Duration::hours(1), LATEST_TIME, StatusCode::CREATED),
        (LATEST_TIME - Duration::hours(1), LATEST_TIME + Duration::seconds(1), StatusCode::BAD_REQUEST),
        (Utc::now() - Duration::hours(2), Utc::now() - Duration::hours(1), StatusCode::BAD_REQUEST),
    ];
    for (starts_at, ends_at, expected) in cases {
        let body = json!({"user_id": alice, "starts_at": starts_at, "ends_at": ends_at});
        let (status, response) = call(&app, "POST", &overrides, Some(&admin), Some(body)).await;
        assert_eq!(status, expected, "{starts_at} .. {ends_at}: {response}");
    }
}

#[sqlx::test]
async fn a_project_holds_at_most_its_schedule_limit(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    sqlx::query(
        "insert into on_call_schedules (project_id, name, timezone, rotation, handoff_time, start_date)
         select $1, 'seeded-' || g, 'UTC', 'daily', '09:00', current_date from generate_series(1, $2::int) g",
    )
    .bind(project)
    .bind(MAX_SCHEDULES_PER_PROJECT - 1)
    .execute(&pool)
    .await
    .unwrap();
    let path = format!("/projects/{project}/schedules");
    let (status, body) = call(&app, "POST", &path, Some(&admin), Some(schedule_body(&[alice]))).await;
    assert_eq!(status, StatusCode::CREATED, "the last place: {body}");
    let mut next = schedule_body(&[alice]);
    next["name"] = json!("one too many");
    let (status, _) = call(&app, "POST", &path, Some(&admin), Some(next)).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn a_schedule_holds_at_most_its_override_limit(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/schedules"), Some(&admin), Some(schedule_body(&[alice]))).await;
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    // Ended overrides do not count.
    sqlx::query(
        "insert into on_call_overrides (schedule_id, user_id, starts_at, ends_at)
         select $1, $2, now() + interval '1 hour', now() + interval '2 hours' from generate_series(1, $3::int)
         union all select $1, $2, now() - interval '2 hours', now() - interval '1 hour'",
    )
    .bind(id)
    .bind(alice)
    .bind(MAX_OVERRIDES_PER_SCHEDULE - 1)
    .execute(&pool)
    .await
    .unwrap();
    let overrides = format!("/projects/{project}/schedules/{id}/overrides");
    let window = json!({"user_id": alice, "starts_at": Utc::now(), "ends_at": Utc::now() + Duration::hours(1)});
    let (status, body) = call(&app, "POST", &overrides, Some(&admin), Some(window.clone())).await;
    assert_eq!(status, StatusCode::CREATED, "the last place: {body}");
    let (status, _) = call(&app, "POST", &overrides, Some(&admin), Some(window)).await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[sqlx::test]
async fn admins_replace_and_delete_schedules(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, alice_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (bob, bob_email, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let path = format!("/projects/{project}/schedules");
    let (_, created) = call(&app, "POST", &path, Some(&admin), Some(schedule_body(&[alice]))).await;
    let one = format!("{path}/{}", created["id"].as_str().unwrap());

    let mut body = schedule_body(&[bob, alice]);
    body["name"] = json!("secondary");
    body["rotation"] = json!("custom");
    body["rotation_hours"] = json!(12);
    let (status, replaced) = call(&app, "PUT", &one, Some(&admin), Some(body.clone())).await;
    assert_eq!(status, StatusCode::OK, "{replaced}");
    assert_eq!(replaced["name"], "secondary");
    assert_eq!(replaced["rotation_hours"], 12);
    let emails: Vec<&str> = replaced["participants"].as_array().unwrap().iter().map(|p| p["email"].as_str().unwrap()).collect();
    assert_eq!(emails, [bob_email.as_str(), alice_email.as_str()], "in the order sent");
    let updated = |v: &Value| v["updated_at"].as_str().unwrap().parse::<DateTime<Utc>>().unwrap();
    assert!(updated(&replaced) > updated(&created));

    let (status, _) = call(&app, "POST", &path, Some(&admin), Some(body)).await;
    assert_eq!(status, StatusCode::CONFLICT, "the name is taken");
    let (_, other) = call(&app, "POST", &path, Some(&admin), Some(schedule_body(&[alice]))).await;
    let mut clash = schedule_body(&[alice]);
    clash["name"] = json!("secondary");
    let (status, _) = call(&app, "PUT", &format!("{path}/{}", other["id"].as_str().unwrap()), Some(&admin), Some(clash.clone())).await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(&app, "PUT", &format!("{path}/{}", Uuid::new_v4()), Some(&admin), Some(schedule_body(&[alice]))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = call(&app, "DELETE", &one, Some(&admin), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    for method in ["GET", "DELETE"] {
        let (status, _) = call(&app, method, &one, Some(&admin), None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method}");
    }
}

#[sqlx::test]
async fn a_replace_may_keep_an_old_start_date_but_a_changed_one_must_be_recent(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let path = format!("/projects/{project}/schedules");
    let (_, created) = call(&app, "POST", &path, Some(&admin), Some(schedule_body(&[alice]))).await;
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let one = format!("{path}/{id}");
    let set_stored = |date: NaiveDate| {
        sqlx::query("update on_call_schedules set start_date = $2 where id = $1").bind(id).bind(date).execute(&pool)
    };
    let old = Utc::now().date_naive() - Months::new(12 * (MAX_START_DATE_YEARS + 2));
    set_stored(old).await.unwrap();

    let mut body = schedule_body(&[alice]);
    body["start_date"] = json!(old);
    let (status, replaced) = call(&app, "PUT", &one, Some(&admin), Some(body.clone())).await;
    assert_eq!((status, replaced["start_date"].clone()), (StatusCode::OK, json!(old)), "kept: {replaced}");
    body["start_date"] = json!(old + Duration::days(1));
    let (status, response) = call(&app, "PUT", &one, Some(&admin), Some(body.clone())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "changed to another old date: {response}");

    let before_range = EARLIEST_TIME.date_naive() - Duration::days(1);
    set_stored(before_range).await.unwrap();
    body["start_date"] = json!(before_range);
    let (status, response) = call(&app, "PUT", &one, Some(&admin), Some(body.clone())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "kept but before the API range: {response}");

    body["name"] = json!("another");
    body["start_date"] = json!(old);
    let (status, response) = call(&app, "POST", &path, Some(&admin), Some(body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "a create gets the five-year rule: {response}");
}

#[sqlx::test]
async fn members_cannot_manage_schedules_and_outsiders_cannot_read_them(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, _, member) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, _, viewer) = user_with_role(&app, &pool, project, Some("viewer")).await;
    let (_, _, outsider) = user_with_role(&app, &pool, project, None).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/schedules"), Some(&admin), Some(schedule_body(&[alice]))).await;
    let one = format!("/projects/{project}/schedules/{}", created["id"].as_str().unwrap());
    let window = json!({"user_id": alice, "starts_at": Utc::now(), "ends_at": Utc::now() + Duration::hours(1)});
    let (_, o) = call(&app, "POST", &format!("{one}/overrides"), Some(&admin), Some(window.clone())).await;

    for (method, uri, body) in [
        ("PUT", one.clone(), Some(schedule_body(&[alice]))),
        ("DELETE", one.clone(), None),
        ("POST", format!("{one}/overrides"), Some(window)),
        ("DELETE", format!("{one}/overrides/{}", o["id"].as_str().unwrap()), None),
    ] {
        let (status, _) = call(&app, method, &uri, Some(&member), body).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
    }
    for uri in [
        format!("/projects/{project}/schedules"),
        one.clone(),
        format!("{one}/shifts"),
        format!("{one}/overrides"),
        format!("/projects/{project}/on-call"),
        format!("/projects/{project}/members"),
    ] {
        let (status, _) = call(&app, "GET", &uri, Some(&outsider), None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "outsider GET {uri}");
        let (status, body) = call(&app, "GET", &uri, Some(&viewer), None).await;
        assert_eq!(status, StatusCode::OK, "viewer GET {uri}: {body}");
    }
}

#[sqlx::test]
async fn nobody_is_on_call_outside_the_restrictions_and_removed_members_are_flagged(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let now = Utc::now();
    let mut body = schedule_body(&[alice]);
    body["timezone"] = json!("UTC");
    body["restrictions"] = json!([{"weekdays": [1,2,3,4,5,6,7],
        "from": (now + Duration::hours(2)).format("%H:%M").to_string(),
        "to": (now + Duration::hours(3)).format("%H:%M").to_string()}]);
    let (status, created) = call(&app, "POST", &format!("/projects/{project}/schedules"), Some(&admin), Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["on_call_now"], Value::Null);
    let one = format!("/projects/{project}/schedules/{}", created["id"].as_str().unwrap());
    let (_, shifts) = call(&app, "GET", &format!("{one}/shifts"), Some(&admin), None).await;
    let first: DateTime<Utc> = shifts[0]["starts_at"].as_str().unwrap().parse().unwrap();
    assert!(first > now + Duration::hours(1), "the first shift is the next window: {shifts}");
    assert_eq!(shifts[0]["is_member"], true);

    sqlx::query("delete from project_members where project_id = $1 and user_id = $2")
        .bind(project)
        .bind(alice)
        .execute(&pool)
        .await
        .unwrap();
    let (_, schedule) = call(&app, "GET", &one, Some(&admin), None).await;
    assert_eq!(schedule["participants"][0]["is_member"], false, "{schedule}");
    let (_, shifts) = call(&app, "GET", &format!("{one}/shifts"), Some(&admin), None).await;
    assert_eq!(shifts[0]["is_member"], false, "listed but flagged (R7)");
}

#[sqlx::test]
async fn load_rotation_reads_the_rotation_and_the_overrides_in_range(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let project = seed_project(&pool).await;
    let (_, _, admin) = user_with_role(&app, &pool, project, Some("admin")).await;
    let (alice, _, _) = user_with_role(&app, &pool, project, Some("member")).await;
    let (_, created) = call(&app, "POST", &format!("/projects/{project}/schedules"), Some(&admin), Some(schedule_body(&[alice]))).await;
    let id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    let current: Uuid = sqlx::query_scalar(
        "insert into on_call_overrides (schedule_id, user_id, starts_at, ends_at)
         values ($1, $2, now() - interval '1 hour', now() + interval '1 hour'),
                ($1, $2, now() - interval '2 hours', now() - interval '1 hour')
         returning id",
    )
    .bind(id)
    .bind(alice)
    .fetch_one(&pool)
    .await
    .unwrap();

    let mut conn = pool.acquire().await.unwrap();
    let now = Utc::now();
    let (rotation, overrides) = oncall::load_rotation(&mut conn, id, now, now).await.unwrap().unwrap();
    assert_eq!((rotation.participants, rotation.length), (vec![alice], RotationLength::Weekly));
    assert_eq!(overrides.iter().map(|o| o.id).collect::<Vec<_>>(), [current], "the ended one is out of range");
    assert!(oncall::load_rotation(&mut conn, Uuid::new_v4(), now, now).await.unwrap().is_none());
}
