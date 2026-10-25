//! Escalation policies, their steps and targets: validation, the SQL, and
//! what the API shows of them.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgExecutor, PgPool};
use uuid::Uuid;

use crate::core::validate::validate_name;
use crate::incidents::timewindow::{TimeWindow, ValidWindow, parse_timezone};

pub const MAX_POLICIES_PER_PROJECT: i64 = 50;
pub const MAX_STEPS: usize = 20;
pub const MAX_TARGETS_PER_STEP: usize = 20;
pub const MAX_REPEAT_COUNT: i32 = 10;
pub const MAX_DELAY_MINUTES: i32 = 1440;

/// Who a step notifies. A `user` must be a project member, a `schedule` and
/// a `project_channel` must be the project's (personal channels are never
/// policy targets).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TargetInput {
    User { id: Uuid },
    Schedule { id: Uuid },
    AllMembers,
    ProjectChannel { id: Uuid },
}

impl TargetInput {
    /// `(kind, user_id, schedule_id, channel_id)` as stored.
    fn columns(&self) -> (&'static str, Option<Uuid>, Option<Uuid>, Option<Uuid>) {
        match *self {
            Self::User { id } => ("user", Some(id), None, None),
            Self::Schedule { id } => ("schedule", None, Some(id), None),
            Self::AllMembers => ("all_members", None, None, None),
            Self::ProjectChannel { id } => ("project_channel", None, None, Some(id)),
        }
    }
}

/// A step runs only inside this weekly window (R19): ISO weekdays 1-7,
/// "HH:MM" times in `timezone`, a `from` after `to` spans midnight.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StepCondition {
    pub weekdays: Vec<u8>,
    pub from: String,
    pub to: String,
    pub timezone: String,
}

impl StepCondition {
    /// The window the condition describes; `Err` says what is wrong with it.
    pub fn window(&self) -> Result<ValidWindow, String> {
        TimeWindow { weekdays: self.weekdays.clone(), from: self.from.clone(), to: self.to.clone() }
            .validate(parse_timezone(&self.timezone)?)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepInput {
    pub delay_minutes: i32,
    #[serde(default)]
    pub condition: Option<StepCondition>,
    pub targets: Vec<TargetInput>,
}

/// The body of a policy create or replace.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyInput {
    pub name: String,
    #[serde(default)]
    pub repeat_count: i32,
    #[serde(default)]
    pub fallback_policy_id: Option<Uuid>,
    pub steps: Vec<StepInput>,
}

/// A target with what the UI shows for it: the user's email, the schedule's
/// name, `"<channel> (<kind>)"` or `"All members"`.
#[derive(Debug, Clone, Serialize)]
pub struct TargetView {
    #[serde(flatten)]
    pub target: TargetInput,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StepView {
    pub position: i32,
    pub delay_minutes: i32,
    pub condition: Option<StepCondition>,
    pub targets: Vec<TargetView>,
}

/// A policy as the API returns it, steps in order.
#[derive(Debug, Clone, Serialize)]
pub struct PolicyView {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub repeat_count: i32,
    pub fallback_policy_id: Option<Uuid>,
    pub is_default: bool,
    pub steps: Vec<StepView>,
    pub updated_at: DateTime<Utc>,
}

/// Shape checks (counts, ranges, duplicates, condition window and zone);
/// `Err` carries a 400's message. Same-project checks happen when saving.
pub fn validate(input: &PolicyInput) -> Result<(), String> {
    validate_name(&input.name)?;
    if !(0..=MAX_REPEAT_COUNT).contains(&input.repeat_count) {
        return Err(format!("repeat_count must be between 0 and {MAX_REPEAT_COUNT}"));
    }
    if !(1..=MAX_STEPS).contains(&input.steps.len()) {
        return Err(format!("steps must list 1 to {MAX_STEPS} steps"));
    }
    for (i, step) in input.steps.iter().enumerate() {
        let n = i + 1;
        if !(0..=MAX_DELAY_MINUTES).contains(&step.delay_minutes) {
            return Err(format!(
                "step {n}: delay_minutes must be between 0 and {MAX_DELAY_MINUTES}"
            ));
        }
        if !(1..=MAX_TARGETS_PER_STEP).contains(&step.targets.len()) {
            return Err(format!("step {n}: targets must list 1 to {MAX_TARGETS_PER_STEP} targets"));
        }
        if step.targets.iter().enumerate().any(|(j, t)| step.targets[..j].contains(t)) {
            return Err(format!("step {n}: each target may be listed once"));
        }
        if let Some(condition) = &step.condition {
            condition.window().map_err(|e| format!("step {n}: condition: {e}"))?;
        }
    }
    Ok(())
}

/// Outcome of [`create`] and [`replace`].
pub enum Saved {
    Saved(PolicyView),
    /// The project (create) or the policy (replace) does not exist.
    NotFound,
    /// The project holds [`MAX_POLICIES_PER_PROJECT`] policies.
    Full,
    /// Another policy of the project has this name.
    NameTaken,
    /// A reference that is not the project's (a 400's message).
    Invalid(String),
}

/// Outcome of [`delete`].
pub enum Deleted {
    Deleted,
    NotFound,
    IsDefault,
}

/// The policies a query reads: `$1` the project and `$2` the policy (null =
/// any), with `p` the policy. A macro because sqlx takes SQL as one literal.
macro_rules! in_scope {
    () => {
        "p.project_id = $1 and ($2::uuid is null or p.id = $2)"
    };
}

#[derive(sqlx::FromRow)]
struct PolicyRow {
    id: Uuid,
    project_id: Uuid,
    name: String,
    repeat_count: i32,
    fallback_policy_id: Option<Uuid>,
    is_default: bool,
    updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct StepRow {
    id: Uuid,
    policy_id: Uuid,
    position: i32,
    delay_minutes: i32,
    condition: Option<sqlx::types::Json<StepCondition>>,
}

#[derive(sqlx::FromRow)]
struct TargetRow {
    step_id: Uuid,
    kind: String,
    user_id: Option<Uuid>,
    schedule_id: Option<Uuid>,
    channel_id: Option<Uuid>,
    label: String,
}

impl TargetRow {
    /// `None` only for a row the table's check refuses.
    fn target(&self) -> Option<TargetInput> {
        Some(match self.kind.as_str() {
            "user" => TargetInput::User { id: self.user_id? },
            "schedule" => TargetInput::Schedule { id: self.schedule_id? },
            "all_members" => TargetInput::AllMembers,
            "project_channel" => TargetInput::ProjectChannel { id: self.channel_id? },
            _ => return None,
        })
    }
}

/// The project's policies (or the one `policy_id`) by name.
async fn load(
    conn: &mut PgConnection,
    project_id: Uuid,
    policy_id: Option<Uuid>,
) -> Result<Vec<PolicyView>, sqlx::Error> {
    let policies: Vec<PolicyRow> = sqlx::query_as(concat!(
        "select p.id, p.project_id, p.name, p.repeat_count, p.fallback_policy_id, p.is_default,
                p.updated_at
         from escalation_policies p where ",
        in_scope!(),
        " order by p.name, p.id"
    ))
    .bind(project_id)
    .bind(policy_id)
    .fetch_all(&mut *conn)
    .await?;
    let steps: Vec<StepRow> = sqlx::query_as(concat!(
        "select s.id, s.policy_id, s.position, s.delay_minutes, s.condition
         from escalation_steps s join escalation_policies p on p.id = s.policy_id
         where ",
        in_scope!(),
        " order by s.policy_id, s.position"
    ))
    .bind(project_id)
    .bind(policy_id)
    .fetch_all(&mut *conn)
    .await?;
    let targets: Vec<TargetRow> = sqlx::query_as(concat!(
        "select t.step_id, t.kind, t.user_id, t.schedule_id, t.channel_id,
                coalesce(u.email, sc.name, c.name || ' (' || c.kind || ')', 'All members') as label
         from escalation_step_targets t
         join escalation_steps s on s.id = t.step_id
         join escalation_policies p on p.id = s.policy_id
         left join users u on u.id = t.user_id
         left join on_call_schedules sc on sc.id = t.schedule_id
         left join notification_channels c on c.id = t.channel_id
         where ",
        in_scope!(),
        " order by t.step_id, t.position"
    ))
    .bind(project_id)
    .bind(policy_id)
    .fetch_all(&mut *conn)
    .await?;

    let mut targets_of: HashMap<Uuid, Vec<TargetView>> = HashMap::new();
    for row in targets {
        if let Some(target) = row.target() {
            targets_of.entry(row.step_id).or_default().push(TargetView { target, label: row.label });
        }
    }
    let mut steps_of: HashMap<Uuid, Vec<StepView>> = HashMap::new();
    for row in steps {
        steps_of.entry(row.policy_id).or_default().push(StepView {
            position: row.position,
            delay_minutes: row.delay_minutes,
            condition: row.condition.map(|c| c.0),
            targets: targets_of.remove(&row.id).unwrap_or_default(),
        });
    }
    Ok(policies
        .into_iter()
        .map(|p| PolicyView {
            steps: steps_of.remove(&p.id).unwrap_or_default(),
            id: p.id,
            project_id: p.project_id,
            name: p.name,
            repeat_count: p.repeat_count,
            fallback_policy_id: p.fallback_policy_id,
            is_default: p.is_default,
            updated_at: p.updated_at,
        })
        .collect())
}

/// The project's policies by name.
pub async fn list(pool: &PgPool, project_id: Uuid) -> Result<Vec<PolicyView>, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    load(&mut conn, project_id, None).await
}

/// One policy of the project; `None`: not in the project.
pub async fn get(pool: &PgPool, project_id: Uuid, id: Uuid) -> Result<Option<PolicyView>, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    Ok(load(&mut conn, project_id, Some(id)).await?.pop())
}

/// Locks the project row, which serialises policy writes in one project (so
/// the policy count and the one default stay exact); `false`: no such project.
async fn lock_project(conn: &mut PgConnection, project_id: Uuid) -> Result<bool, sqlx::Error> {
    let found: Option<Uuid> =
        sqlx::query_scalar("select id from projects where id = $1 for no key update")
            .bind(project_id)
            .fetch_optional(conn)
            .await?;
    Ok(found.is_some())
}

// The first of `$2` (in order) that is not a member, schedule or project
// channel of project `$1`.
const FIRST_NON_MEMBER: &str = "select t.u from unnest($2::uuid[]) with ordinality as t(u, n)
    where not exists (select 1 from project_members m where m.project_id = $1 and m.user_id = t.u)
    order by t.n limit 1";
const FIRST_FOREIGN_SCHEDULE: &str = "select t.u from unnest($2::uuid[]) with ordinality as t(u, n)
    where not exists (select 1 from on_call_schedules s where s.project_id = $1 and s.id = t.u)
    order by t.n limit 1";
const FIRST_FOREIGN_CHANNEL: &str = "select t.u from unnest($2::uuid[]) with ordinality as t(u, n)
    where not exists (select 1 from notification_channels c where c.project_id = $1 and c.id = t.u)
    order by t.n limit 1";

async fn first_missing(
    conn: &mut PgConnection,
    sql: &'static str,
    project_id: Uuid,
    ids: &[Uuid],
) -> Result<Option<Uuid>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(None);
    }
    sqlx::query_scalar(sql).bind(project_id).bind(ids).fetch_optional(conn).await
}

/// The same-project checks of a save (`own_id`: the policy being replaced);
/// `Some` carries a 400's message.
async fn check_references(
    conn: &mut PgConnection,
    project_id: Uuid,
    own_id: Option<Uuid>,
    input: &PolicyInput,
) -> Result<Option<String>, sqlx::Error> {
    if let Some(fallback) = input.fallback_policy_id {
        if own_id == Some(fallback) {
            return Ok(Some("a policy cannot fall back to itself".into()));
        }
        if !belongs_to(&mut *conn, project_id, fallback).await? {
            return Ok(Some("fallback policy not found in this project".into()));
        }
    }
    let targets = || input.steps.iter().flat_map(|s| s.targets.iter());
    let ids = |pick: fn(&TargetInput) -> Option<Uuid>| targets().filter_map(pick).collect::<Vec<_>>();
    let users = ids(|t| match t {
        TargetInput::User { id } => Some(*id),
        _ => None,
    });
    if let Some(user) = first_missing(conn, FIRST_NON_MEMBER, project_id, &users).await? {
        return Ok(Some(format!("user {user} is not a member of this project")));
    }
    let schedules = ids(|t| match t {
        TargetInput::Schedule { id } => Some(*id),
        _ => None,
    });
    if let Some(id) = first_missing(conn, FIRST_FOREIGN_SCHEDULE, project_id, &schedules).await? {
        return Ok(Some(format!("schedule {id} not found in this project")));
    }
    let channels = ids(|t| match t {
        TargetInput::ProjectChannel { id } => Some(*id),
        _ => None,
    });
    if let Some(id) = first_missing(conn, FIRST_FOREIGN_CHANNEL, project_id, &channels).await? {
        return Ok(Some(format!("channel {id} is not a channel of this project")));
    }
    Ok(None)
}

/// Inserts the steps (positions 0..n-1) and their targets (in order).
async fn write_steps(
    conn: &mut PgConnection,
    policy_id: Uuid,
    steps: &[StepInput],
) -> Result<(), sqlx::Error> {
    for (position, step) in steps.iter().enumerate() {
        let condition = step.condition.clone().map(|mut c| {
            c.weekdays.sort_unstable();
            sqlx::types::Json(c)
        });
        let step_id: Uuid = sqlx::query_scalar(
            "insert into escalation_steps (policy_id, position, delay_minutes, condition)
             values ($1, $2, $3, $4) returning id",
        )
        .bind(policy_id)
        .bind(position as i32)
        .bind(step.delay_minutes)
        .bind(condition)
        .fetch_one(&mut *conn)
        .await?;
        let mut kinds = Vec::with_capacity(step.targets.len());
        let mut users = Vec::with_capacity(step.targets.len());
        let mut schedules = Vec::with_capacity(step.targets.len());
        let mut channels = Vec::with_capacity(step.targets.len());
        for target in &step.targets {
            let (kind, user, schedule, channel) = target.columns();
            kinds.push(kind);
            users.push(user);
            schedules.push(schedule);
            channels.push(channel);
        }
        sqlx::query(
            "insert into escalation_step_targets (step_id, position, kind, user_id, schedule_id, channel_id)
             select $1, (t.n - 1)::int, t.kind, t.user_id, t.schedule_id, t.channel_id
             from unnest($2::text[], $3::uuid[], $4::uuid[], $5::uuid[])
                  with ordinality as t(kind, user_id, schedule_id, channel_id, n)",
        )
        .bind(step_id)
        .bind(&kinds)
        .bind(&users)
        .bind(&schedules)
        .bind(&channels)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    matches!(e, sqlx::Error::Database(db) if db.is_unique_violation())
}

/// Creates a policy (checked by [`validate`]) in the project.
pub async fn create(pool: &PgPool, project_id: Uuid, input: &PolicyInput) -> Result<Saved, sqlx::Error> {
    let mut tx = pool.begin().await?;
    if !lock_project(&mut tx, project_id).await? {
        return Ok(Saved::NotFound);
    }
    let count: i64 = sqlx::query_scalar("select count(*) from escalation_policies where project_id = $1")
        .bind(project_id)
        .fetch_one(&mut *tx)
        .await?;
    if count >= MAX_POLICIES_PER_PROJECT {
        return Ok(Saved::Full);
    }
    if let Some(message) = check_references(&mut tx, project_id, None, input).await? {
        return Ok(Saved::Invalid(message));
    }
    let inserted = sqlx::query_scalar(
        "insert into escalation_policies (project_id, name, repeat_count, fallback_policy_id)
         values ($1, $2, $3, $4) returning id",
    )
    .bind(project_id)
    .bind(input.name.trim())
    .bind(input.repeat_count)
    .bind(input.fallback_policy_id)
    .fetch_one(&mut *tx)
    .await;
    let id: Uuid = match inserted {
        Err(e) if is_unique_violation(&e) => return Ok(Saved::NameTaken),
        inserted => inserted?,
    };
    write_steps(&mut tx, id, &input.steps).await?;
    let view = load(&mut tx, project_id, Some(id)).await?.pop();
    tx.commit().await?;
    Ok(view.map_or(Saved::NotFound, Saved::Saved))
}

/// Replaces a policy's name, repeat count, fallback and all its steps.
pub async fn replace(
    pool: &PgPool,
    project_id: Uuid,
    id: Uuid,
    input: &PolicyInput,
) -> Result<Saved, sqlx::Error> {
    let mut tx = pool.begin().await?;
    if !lock_project(&mut tx, project_id).await? || !belongs_to(&mut *tx, project_id, id).await? {
        return Ok(Saved::NotFound);
    }
    if let Some(message) = check_references(&mut tx, project_id, Some(id), input).await? {
        return Ok(Saved::Invalid(message));
    }
    let updated = sqlx::query(
        "update escalation_policies
         set name = $2, repeat_count = $3, fallback_policy_id = $4, updated_at = now()
         where id = $1",
    )
    .bind(id)
    .bind(input.name.trim())
    .bind(input.repeat_count)
    .bind(input.fallback_policy_id)
    .execute(&mut *tx)
    .await;
    match updated {
        Err(e) if is_unique_violation(&e) => return Ok(Saved::NameTaken),
        updated => updated?,
    };
    sqlx::query("delete from escalation_steps where policy_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    write_steps(&mut tx, id, &input.steps).await?;
    let view = load(&mut tx, project_id, Some(id)).await?.pop();
    tx.commit().await?;
    Ok(view.map_or(Saved::NotFound, Saved::Saved))
}

/// Makes the policy the project's default (incidents of sources without a
/// policy of their own use it); `false`: not in the project.
pub async fn make_default(pool: &PgPool, project_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
    let mut tx = pool.begin().await?;
    if !lock_project(&mut tx, project_id).await? || !belongs_to(&mut *tx, project_id, id).await? {
        return Ok(false);
    }
    // Two statements: the one-default index is checked row by row.
    sqlx::query(
        "update escalation_policies set is_default = false, updated_at = now()
         where project_id = $1 and is_default and id <> $2",
    )
    .bind(project_id)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "update escalation_policies set is_default = true, updated_at = now()
         where id = $1 and not is_default",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Deletes a policy that is not the default. Policies falling back to it
/// lose their fallback, and incidents walking it lose their policy (the
/// escalation engine ends their escalation).
pub async fn delete(pool: &PgPool, project_id: Uuid, id: Uuid) -> Result<Deleted, sqlx::Error> {
    let mut tx = pool.begin().await?;
    if !lock_project(&mut tx, project_id).await? {
        return Ok(Deleted::NotFound);
    }
    let is_default: Option<bool> = sqlx::query_scalar(
        "select is_default from escalation_policies where id = $1 and project_id = $2",
    )
    .bind(id)
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?;
    match is_default {
        None => return Ok(Deleted::NotFound),
        Some(true) => return Ok(Deleted::IsDefault),
        Some(false) => {}
    }
    sqlx::query("delete from escalation_policies where id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Deleted::Deleted)
}

/// Whether `policy_id` is one of the project's policies.
pub async fn belongs_to<'e>(
    executor: impl PgExecutor<'e>,
    project_id: Uuid,
    policy_id: Uuid,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "select exists (select 1 from escalation_policies where id = $1 and project_id = $2)",
    )
    .bind(policy_id)
    .bind(project_id)
    .fetch_one(executor)
    .await
}

/// The name of a policy of the project with a step that targets the
/// schedule, if any (a targeted schedule cannot be deleted).
pub async fn policy_targeting_schedule<'e>(
    executor: impl PgExecutor<'e>,
    project_id: Uuid,
    schedule_id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "select p.name from escalation_step_targets t
         join escalation_steps s on s.id = t.step_id
         join escalation_policies p on p.id = s.policy_id
         where t.schedule_id = $2 and p.project_id = $1
         order by p.name limit 1",
    )
    .bind(project_id)
    .bind(schedule_id)
    .fetch_optional(executor)
    .await
}

/// The name of a policy of the project with a step that targets the
/// channel, if any (a targeted channel cannot be deleted).
pub async fn policy_targeting_channel<'e>(
    executor: impl PgExecutor<'e>,
    project_id: Uuid,
    channel_id: Uuid,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar(
        "select p.name from escalation_step_targets t
         join escalation_steps s on s.id = t.step_id
         join escalation_policies p on p.id = s.policy_id
         where t.channel_id = $2 and p.project_id = $1
         order by p.name limit 1",
    )
    .bind(project_id)
    .bind(channel_id)
    .fetch_optional(executor)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(delay_minutes: i32, targets: Vec<TargetInput>) -> StepInput {
        StepInput { delay_minutes, condition: None, targets }
    }
    fn policy(repeat_count: i32, steps: Vec<StepInput>) -> PolicyInput {
        PolicyInput { name: "p".into(), repeat_count, fallback_policy_id: None, steps }
    }
    fn users(n: usize) -> Vec<TargetInput> {
        (0..n).map(|i| TargetInput::User { id: Uuid::from_u128(i as u128 + 1) }).collect()
    }
    fn all() -> Vec<TargetInput> {
        vec![TargetInput::AllMembers]
    }

    #[test]
    fn counts_and_ranges_are_bounded() {
        let steps = |n: usize| (0..n).map(|_| step(0, all())).collect::<Vec<_>>();
        for (input, ok) in [
            (policy(0, steps(MAX_STEPS)), true),
            (policy(0, steps(MAX_STEPS + 1)), false),
            (policy(0, steps(0)), false),
            (policy(MAX_REPEAT_COUNT, steps(1)), true),
            (policy(MAX_REPEAT_COUNT + 1, steps(1)), false),
            (policy(-1, steps(1)), false),
            (policy(0, vec![step(MAX_DELAY_MINUTES, all())]), true),
            (policy(0, vec![step(MAX_DELAY_MINUTES + 1, all())]), false),
            (policy(0, vec![step(-1, all())]), false),
            (policy(0, vec![step(0, users(MAX_TARGETS_PER_STEP))]), true),
            (policy(0, vec![step(0, users(MAX_TARGETS_PER_STEP + 1))]), false),
            (policy(0, vec![step(0, vec![])]), false),
        ] {
            assert_eq!(validate(&input).is_ok(), ok, "{input:?}");
        }
    }

    #[test]
    fn a_step_lists_each_target_once_and_conditions_are_checked() {
        let twice = vec![TargetInput::Schedule { id: Uuid::nil() }, TargetInput::Schedule { id: Uuid::nil() }];
        assert!(validate(&policy(0, vec![step(0, twice)])).is_err());
        let condition = |timezone: &str, from: &str| StepCondition {
            weekdays: vec![5, 1],
            from: from.into(),
            to: "17:00".into(),
            timezone: timezone.into(),
        };
        let with = |c| policy(0, vec![StepInput { delay_minutes: 0, condition: Some(c), targets: all() }]);
        assert!(validate(&with(condition("Asia/Jakarta", "09:00"))).is_ok());
        assert!(validate(&with(condition("Nowhere/Land", "09:00"))).is_err());
        assert!(validate(&with(condition("Asia/Jakarta", "9:00"))).is_err());
        assert!(validate(&PolicyInput { name: " ".into(), ..policy(0, vec![step(0, all())]) }).is_err());
    }
}
