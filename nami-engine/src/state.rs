use sqlx::PgPool;

use crate::{config::Config, email};

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
    pub mailer: email::Mailer,
    pub config: Config,
}
