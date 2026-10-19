/// A per-project table a member can add rows to through the API, and the
/// most rows a project may hold in it. Without a cap one member could create
/// rows without end, and every read of the project (and, for maintenance
/// windows, every check result and heartbeat sweep) loads them all.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ProjectQuota {
    MaintenanceWindows,
    MonitorGroups,
}

impl ProjectQuota {
    pub(crate) const MAX_ROWS: i64 = 200;

    fn count_query(self) -> &'static str {
        match self {
            ProjectQuota::MaintenanceWindows => {
                "select count(*) from maintenance_windows where project_id = $1"
            }
            ProjectQuota::MonitorGroups => {
                "select count(*) from monitor_groups where project_id = $1"
            }
        }
    }

    fn label(self) -> &'static str {
        match self {
            ProjectQuota::MaintenanceWindows => "maintenance windows",
            ProjectQuota::MonitorGroups => "monitor groups",
        }
    }

    /// Takes the project's row lock (`for no key update`: concurrent creates
    /// of the same project queue up behind it, foreign-key checks of other
    /// writes do not) and counts the project's rows, so the count holds until
    /// the caller's transaction commits its insert. A project that does not
    /// exist is a 404 (a global admin skips membership); a full one a 400.
    pub(crate) async fn check(
        self,
        conn: &mut sqlx::PgConnection,
        project_id: uuid::Uuid,
    ) -> Result<(), crate::core::error::AppError> {
        let exists: Option<uuid::Uuid> =
            sqlx::query_scalar("select id from projects where id = $1 for no key update")
                .bind(project_id)
                .fetch_optional(&mut *conn)
                .await?;
        if exists.is_none() {
            return Err(crate::core::error::AppError::NotFound);
        }
        let count: i64 = sqlx::query_scalar(self.count_query())
            .bind(project_id)
            .fetch_one(&mut *conn)
            .await?;
        if count >= Self::MAX_ROWS {
            return Err(crate::core::error::AppError::Validation(format!(
                "a project can have at most {} {}: delete one first",
                Self::MAX_ROWS,
                self.label()
            )));
        }
        Ok(())
    }
}
