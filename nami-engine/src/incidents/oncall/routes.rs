//! On-call schedules: viewers read schedules, shifts, overrides and who is
//! on call; admins manage schedules and overrides (R25).

use axum::{
    Json, Router,
    extract::{Path, Query, State, rejection::QueryRejection},
    http::StatusCode,
    routing::{delete, get},
};
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    core::error::AppError,
    core::middleware::rbac::{CurrentUser, Role, require_project_role},
    core::state::AppState,
    incidents::oncall::model::{
        self as schedule, DEFAULT_SHIFT_DAYS, Deleted, Loaded, OnCallNow, OverrideView,
        ScheduleInput, ScheduleView, ShiftView,
    },
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/schedules", get(list).post(create))
        .route(
            "/projects/{id}/schedules/{schedule_id}",
            get(get_one).put(replace).delete(remove),
        )
        .route("/projects/{id}/schedules/{schedule_id}/shifts", get(shifts))
        .route(
            "/projects/{id}/schedules/{schedule_id}/overrides",
            get(list_overrides).post(add_override),
        )
        .route(
            "/projects/{id}/schedules/{schedule_id}/overrides/{override_id}",
            delete(delete_override),
        )
        .route("/projects/{id}/on-call", get(board))
}

/// A schedule and who is on call in it now.
#[derive(Serialize)]
struct ScheduleReply {
    #[serde(flatten)]
    schedule: ScheduleView,
    on_call_now: Option<OnCallNow>,
}

/// One row of the project dashboard's "on call now" card.
#[derive(Serialize)]
struct BoardEntry {
    schedule_id: Uuid,
    name: String,
    on_call_now: Option<OnCallNow>,
}

#[derive(Deserialize)]
struct ShiftsQuery {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OverrideInput {
    user_id: Uuid,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
}

/// Runs the rotation math on a blocking thread, never an async worker: a
/// 14-day look-ahead is ~20k evaluations per schedule. A panic there is
/// re-raised here.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(work).await {
        Ok(value) => value,
        Err(error) => std::panic::resume_unwind(error.into_panic()),
    }
}

/// The overrides that "on call now" needs end after `now` and start before this.
fn look_ahead(now: DateTime<Utc>) -> DateTime<Utc> {
    now + TimeDelta::days(DEFAULT_SHIFT_DAYS)
}

async fn replies(loaded: Vec<Loaded>, now: DateTime<Utc>) -> Vec<ScheduleReply> {
    blocking(move || {
        loaded
            .into_iter()
            .map(|l| ScheduleReply { on_call_now: l.on_call_now(now), schedule: l.view })
            .collect()
    })
    .await
}

/// The reply for one schedule; 404 when it is not (or no longer) in the
/// project.
async fn one(state: &AppState, project_id: Uuid, id: Uuid) -> Result<ScheduleReply, AppError> {
    let now = Utc::now();
    let loaded = schedule::get(&state.db, project_id, id, now, look_ahead(now))
        .await?
        .ok_or(AppError::NotFound)?;
    replies(vec![loaded], now).await.pop().ok_or(AppError::NotFound)
}

async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<ScheduleReply>>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    let now = Utc::now();
    let loaded = schedule::list(&state.db, project_id, now, look_ahead(now)).await?;
    Ok(Json(replies(loaded, now).await))
}

async fn create(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
    Json(input): Json<ScheduleInput>,
) -> Result<(StatusCode, Json<ScheduleReply>), AppError> {
    require_project_role(&state.db, &user, project_id, Role::Admin).await?;
    let valid = schedule::validate(input).map_err(AppError::Validation)?;
    let id = schedule::create(&state.db, project_id, &valid).await?;
    Ok((StatusCode::CREATED, Json(one(&state, project_id, id).await?)))
}

async fn get_one(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, id)): Path<(Uuid, Uuid)>,
) -> Result<Json<ScheduleReply>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    Ok(Json(one(&state, project_id, id).await?))
}

async fn replace(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, id)): Path<(Uuid, Uuid)>,
    Json(input): Json<ScheduleInput>,
) -> Result<Json<ScheduleReply>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Admin).await?;
    let valid = schedule::validate(input).map_err(AppError::Validation)?;
    if !schedule::replace(&state.db, project_id, id, &valid).await? {
        return Err(AppError::NotFound);
    }
    Ok(Json(one(&state, project_id, id).await?))
}

async fn remove(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Admin).await?;
    match schedule::delete(&state.db, project_id, id).await? {
        Deleted::Deleted => Ok(StatusCode::NO_CONTENT),
        Deleted::NotFound => Err(AppError::NotFound),
        Deleted::InUse { policy } => Err(AppError::Conflict(format!(
            "schedule is used by escalation policy \"{policy}\""
        ))),
    }
}

async fn shifts(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, id)): Path<(Uuid, Uuid)>,
    query: Result<Query<ShiftsQuery>, QueryRejection>,
) -> Result<Json<Vec<ShiftView>>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    // A time that does not parse answers in the API's own JSON shape.
    let Query(query) = query
        .map_err(|_| AppError::Validation("from and to must be RFC 3339 times".into()))?;
    let (from, to) =
        schedule::shift_range(query.from, query.to, Utc::now()).map_err(AppError::Validation)?;
    let loaded = schedule::get(&state.db, project_id, id, from, to)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(Json(blocking(move || loaded.shifts(from, to)).await))
}

async fn list_overrides(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Vec<OverrideView>>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    schedule::list_overrides(&state.db, project_id, id)
        .await?
        .map(Json)
        .ok_or(AppError::NotFound)
}

async fn add_override(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, id)): Path<(Uuid, Uuid)>,
    Json(input): Json<OverrideInput>,
) -> Result<(StatusCode, Json<OverrideView>), AppError> {
    require_project_role(&state.db, &user, project_id, Role::Admin).await?;
    schedule::check_override(input.starts_at, input.ends_at, Utc::now())
        .map_err(AppError::Validation)?;
    let created = schedule::add_override(
        &state.db,
        project_id,
        id,
        input.user_id,
        input.starts_at,
        input.ends_at,
        user.id,
    )
    .await?
    .ok_or(AppError::NotFound)?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn delete_override(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((project_id, id, override_id)): Path<(Uuid, Uuid, Uuid)>,
) -> Result<StatusCode, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Admin).await?;
    if schedule::delete_override(&state.db, project_id, id, override_id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::NotFound)
    }
}

async fn board(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(project_id): Path<Uuid>,
) -> Result<Json<Vec<BoardEntry>>, AppError> {
    require_project_role(&state.db, &user, project_id, Role::Viewer).await?;
    let now = Utc::now();
    let loaded = schedule::list(&state.db, project_id, now, look_ahead(now)).await?;
    let entries = blocking(move || {
        loaded
            .into_iter()
            .map(|l| BoardEntry {
                on_call_now: l.on_call_now(now),
                schedule_id: l.view.id,
                name: l.view.name,
            })
            .collect()
    })
    .await;
    Ok(Json(entries))
}
