//! Signed, single-use acknowledge/resolve links (R16): what a link would do
//! ([`inspect`], never writes), doing it once ([`confirm`]), and forgetting
//! spent tokens long after they expired ([`prune_spent`]).

pub mod routes;
pub mod token;

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::{
    core::{crypto::Cipher, middleware::rbac::Role},
    incidents::lifecycle,
};
use token::{Action, Claims, TOKEN_TTL};

/// How long the hash of a spent token is kept after the token expired.
pub const SPENT_KEPT_FOR: Duration = Duration::from_secs(2 * 24 * 3600);

/// Why a link did nothing.
#[derive(Debug)]
pub enum Refused {
    /// Malformed, forged or expired; an unknown incident or user; or signed
    /// before the user's last password change.
    Invalid,
    /// The user is neither a project member/admin nor a global admin.
    NoAccess,
    /// The token was already used.
    Used,
    /// The action no longer applies (the incident's status). The token stays unused.
    AlreadyDone(String),
    Database(sqlx::Error),
}

impl From<sqlx::Error> for Refused {
    fn from(e: sqlx::Error) -> Self {
        Refused::Database(e)
    }
}

/// The incident a link acts on, as the confirmation page shows it.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct LinkIncident {
    pub id: Uuid,
    pub project_id: Uuid,
    pub project_name: String,
    pub title: String,
    pub status: String,
    pub severity: String,
    pub started_at: DateTime<Utc>,
}

/// What a link would do, and for whom.
#[derive(Debug, Serialize)]
pub struct Inspected {
    pub action: &'static str,
    pub incident: LinkIncident,
    pub user_email: String,
}

#[derive(sqlx::FromRow)]
struct Loaded {
    #[sqlx(flatten)]
    incident: LinkIncident,
    user_email: String,
    global_role: String,
    project_role: Option<String>,
    revoked: bool,
}

// $1 = incident, $2 = user, $3 = when the token was signed. `exp` is a unix
// second, so the password change is compared at whole seconds too: a link
// signed in the second an account was created still works.
const LOAD: &str = "select i.id, i.project_id, p.name as project_name, i.title, i.status,
        coalesce(s.name, 'unknown') as severity, i.started_at,
        u.email as user_email, u.global_role, pm.project_role,
        $3 < date_trunc('second', u.password_changed_at) as revoked
    from incidents i
    join projects p on p.id = i.project_id
    join users u on u.id = $2
    left join incident_severities s on s.id = i.severity_id
    left join project_members pm on pm.project_id = i.project_id and pm.user_id = u.id
    where i.id = $1";

/// A global admin, or a project member or admin. Unknown roles fail closed.
fn may_act(global_role: &str, project_role: Option<&str>) -> bool {
    Role::parse(global_role) == Some(Role::Admin)
        || project_role.and_then(Role::parse).is_some_and(|role| role >= Role::Member)
}

/// Steps 2-3 of R16: the incident and the user exist, the token predates no
/// password change, and the user may still act on the project.
async fn check(conn: &mut PgConnection, claims: &Claims) -> Result<Loaded, Refused> {
    let loaded: Option<Loaded> = sqlx::query_as(LOAD)
        .bind(claims.incident_id)
        .bind(claims.user_id)
        .bind(claims.expires_at - TOKEN_TTL)
        .fetch_optional(&mut *conn)
        .await?;
    let loaded = loaded.filter(|l| !l.revoked).ok_or(Refused::Invalid)?;
    if !may_act(&loaded.global_role, loaded.project_role.as_deref()) {
        return Err(Refused::NoAccess);
    }
    Ok(loaded)
}

async fn used(conn: &mut PgConnection, hash: &[u8; 32]) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("select exists(select 1 from incident_action_token_uses where token_hash = $1)")
        .bind(&hash[..])
        .fetch_one(conn)
        .await
}

/// What `token` would do at `now`. Read-only: a mail scanner or link preview
/// that loads the page changes nothing. A used token is `Used`.
pub async fn inspect(pool: &PgPool, cipher: &Cipher, token: &str, now: DateTime<Utc>) -> Result<Inspected, Refused> {
    let claims = token::verify(cipher, token, now).map_err(|_| Refused::Invalid)?;
    let mut conn = pool.acquire().await?;
    let loaded = check(&mut conn, &claims).await?;
    if used(&mut conn, &token::hash(token)).await? {
        return Err(Refused::Used);
    }
    Ok(Inspected { action: claims.action.as_str(), incident: loaded.incident, user_email: loaded.user_email })
}

/// Does what `token` says, as its user, once, and returns the new status. In
/// one transaction, in R16's order: verify (no database work for a bad
/// token), lock the incident, check the user, refuse a used token, act (an
/// action that no longer applies rolls back and leaves the token unused),
/// then store the token's hash (a duplicate rolls the action back). Two
/// confirms of one token serialise on the row lock; the hash's primary key
/// backs that up.
pub async fn confirm(
    pool: &PgPool,
    cipher: &Cipher,
    token: &str,
    now: DateTime<Utc>,
) -> Result<&'static str, Refused> {
    let claims = token::verify(cipher, token, now).map_err(|_| Refused::Invalid)?;
    let mut tx = pool.begin().await?;
    let locked: Option<i32> = sqlx::query_scalar("select 1 from incidents where id = $1 for update")
        .bind(claims.incident_id)
        .fetch_optional(&mut *tx)
        .await?;
    if locked.is_none() {
        return Err(Refused::Invalid);
    }
    let loaded = check(&mut tx, &claims).await?;
    let hash = token::hash(token);
    if used(&mut tx, &hash).await? {
        return Err(Refused::Used);
    }
    let (done, status) = match claims.action {
        Action::Acknowledge => {
            (lifecycle::acknowledge(&mut tx, claims.incident_id, claims.user_id, now).await?, "acknowledged")
        }
        Action::Resolve => {
            (lifecycle::resolve(&mut tx, claims.incident_id, Some(claims.user_id), now).await?, "resolved")
        }
    };
    if !done {
        return Err(Refused::AlreadyDone(loaded.incident.status));
    }
    let stored: Option<Vec<u8>> = sqlx::query_scalar(
        "insert into incident_action_token_uses (token_hash, incident_id, expires_at) values ($1, $2, $3)
         on conflict do nothing returning token_hash",
    )
    .bind(&hash[..])
    .bind(claims.incident_id)
    .bind(claims.expires_at)
    .fetch_optional(&mut *tx)
    .await?;
    if stored.is_none() {
        return Err(Refused::Used);
    }
    tx.commit().await?;
    Ok(status)
}

/// Deletes the hashes of tokens that expired more than [`SPENT_KEPT_FOR`]
/// ago (an expired token is refused anyway). Returns how many.
pub async fn prune_spent(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let deleted = sqlx::query(
        "delete from incident_action_token_uses where expires_at < now() - make_interval(secs => $1)",
    )
    .bind(SPENT_KEPT_FOR.as_secs_f64())
    .execute(pool)
    .await?
    .rows_affected();
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_members_admins_and_global_admins_may_act() {
        assert!(may_act("admin", None));
        assert!(may_act("member", Some("member")));
        assert!(may_act("viewer", Some("admin")));
        assert!(!may_act("member", Some("viewer")));
        assert!(!may_act("member", None));
        assert!(!may_act("owner", Some("owner")), "unknown roles fail closed");
    }
}
