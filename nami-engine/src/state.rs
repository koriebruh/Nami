use sqlx::PgPool;

use crate::email;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub mailer: email::Mailer,
}
