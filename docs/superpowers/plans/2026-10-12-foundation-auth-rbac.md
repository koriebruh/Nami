# Foundation: Auth, Sessions, RBAC Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `nami-engine` (Rust/Axum) auth: registration, email verification, password login with optional TOTP 2FA, OAuth (Google/GitHub), password reset, server-side sessions, and two-level RBAC (global role + per-project role) — plus the `nami-ui` BFF wiring that proxies to it.

**Architecture:** `nami-ui` (SvelteKit) is a backend-for-frontend that proxies every auth call to `nami-engine` (Axum) over the internal docker network and owns the session cookie on its own origin. `nami-engine` owns Postgres (users, sessions, RBAC) and Mailpit (verification/reset emails). ClickHouse is provisioned but unused until the next sub-project.

**Tech Stack:** Rust, Axum, SQLx (Postgres), tower-sessions + tower-sessions-sqlx-store, argon2, totp-rs, oauth2, lettre, tower_governor, SvelteKit 5.

**Spec:** `docs/superpowers/specs/2026-10-12-foundation-auth-rbac-design.md`

## Global Constraints

- Single-org only — no multi-tenancy, no `org_id` scoping.
- Sessions are server-side only (tower-sessions + Postgres store). No JWT anywhere.
- Passwords hashed with argon2 only.
- Verification and reset tokens are stored as a hash (sha256), never raw, in the database.
- All auth error responses are generic ("invalid email or password" / "if this email exists, a link was sent") — no response shape, status, or timing difference may reveal whether an email is registered.
- RBAC has exactly two levels: `global_role` on `users` (`admin`/`member`/`viewer`) and `project_role` on `project_members` (`admin`/`member`/`viewer`). Global `admin` bypasses project membership checks; global `member`/`viewer` without a `project_members` row for a project gets no access to that project.
- Authorization failure on any route returns `403`, never `404`.
- OAuth providers are Google and GitHub only.
- `/auth/register` and `/auth/login` are rate-limited per IP.

## Review Focus

- Registering twice with the same email must not create a duplicate user or reveal the account already exists → Task 6.
- An expired or already-consumed password-reset token must be rejected, not accepted → Task 9.
- Logging in with the correct password on an OAuth-only account (`password_hash` is `NULL`) must return the generic 401, not panic or 500 → Task 7.
- A login with TOTP enabled but a missing or wrong code must never produce a working session → Task 8.
- A user with no `project_members` row for a project (and not global admin) must get 403 from a project-scoped route, not 404 or a silent pass → Task 12.

---

## File Structure

```
nami-engine/
  Cargo.toml
  .env.example
  docker-compose.yaml
  migrations/
    0001_create_users.sql
    0002_create_oauth_identities.sql
    0003_create_projects.sql
    0004_create_project_members.sql
    0005_create_tokens.sql
    0006_add_password_changed_at.sql
  src/
    main.rs
    config.rs
    db.rs
    state.rs
    error.rs
    auth/
      mod.rs
      password.rs
      token.rs
      totp.rs
      oauth.rs
    email.rs
    models/
      mod.rs
      user.rs
      project.rs
      token.rs
    middleware/
      mod.rs
      rbac.rs
    routes/
      mod.rs
      health.rs
      auth.rs
      projects.rs
  tests/
    health.rs
    migrations.rs
    email.rs
    register.rs
    login.rs
    totp.rs
    password_reset.rs
    oauth.rs
    rbac.rs
    rate_limit.rs

nami-ui/
  .env.example
  src/
    hooks.server.ts
    lib/server/engine.ts
    routes/
      register/+page.svelte
      register/+page.server.ts
      login/+page.svelte
      login/+page.server.ts
  src/lib/server/engine.test.ts
```

---

### Task 1: Engine bootstrap — config, health check, docker services

**Files:**
- Modify: `nami-engine/Cargo.toml`
- Create: `nami-engine/.env.example`
- Modify: `nami-engine/docker-compose.yaml`
- Create: `nami-engine/src/config.rs`
- Create: `nami-engine/src/error.rs`
- Create: `nami-engine/src/db.rs`
- Create: `nami-engine/src/state.rs`
- Create: `nami-engine/src/routes/mod.rs`
- Create: `nami-engine/src/routes/health.rs`
- Modify: `nami-engine/src/main.rs`
- Create: `nami-engine/migrations/.gitkeep`
- Test: `nami-engine/tests/health.rs`

**Interfaces:**
- Produces: `Config::from_env() -> Config` (fields: `database_url: String`, `port: u16`), `AppError` enum implementing `axum::response::IntoResponse`, `AppState { db: sqlx::PgPool }` (Clone), `db::connect_pool(database_url: &str) -> Result<sqlx::PgPool, sqlx::Error>`, `routes::health::router() -> axum::Router<AppState>`.

- [ ] **Step 1: Add core dependencies**

```bash
cd nami-engine
cargo add axum
cargo add tokio --features full
cargo add sqlx --features runtime-tokio,postgres,uuid,chrono,migrate
cargo add tower-http --features trace
cargo add serde --features derive
cargo add serde_json
cargo add uuid --features v4,serde
cargo add chrono --features serde
cargo add tracing
cargo add tracing-subscriber
cargo add dotenvy
cargo add thiserror
```

- [ ] **Step 2: Write `.env.example`**

```
DATABASE_URL=postgres://nami:nami@localhost:5432/nami
PORT=8080
```

- [ ] **Step 3: Replace `docker-compose.yaml` placeholder content**

```yaml
services:
  postgres:
    image: postgres:17
    environment:
      POSTGRES_USER: nami
      POSTGRES_PASSWORD: nami
      POSTGRES_DB: nami
    ports:
      - "5432:5432"
    volumes:
      - nami_postgres_data:/var/lib/postgresql/data

  mailpit:
    image: axllent/mailpit:latest
    ports:
      - "1025:1025" # SMTP
      - "8025:8025" # Web UI + API

  clickhouse:
    image: clickhouse/clickhouse-server:latest
    ports:
      - "8123:8123"
      - "9000:9000"
    # unused until the uptime-monitoring sub-project

volumes:
  nami_postgres_data:
```

- [ ] **Step 4: Write `src/error.rs`**

```rust
use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("database error")]
    Database(#[from] sqlx::Error),
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("validation error: {0}")]
    Validation(String),
    #[error("not found")]
    NotFound,
    #[error("rate limited")]
    RateLimited,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            AppError::Database(e) => {
                tracing::error!(error = %e, "database error");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string())
            }
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_string()),
            AppError::Forbidden => (StatusCode::FORBIDDEN, "forbidden".to_string()),
            AppError::Validation(msg) => (StatusCode::BAD_REQUEST, msg.clone()),
            AppError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            AppError::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "too many requests".to_string()),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}
```

- [ ] **Step 5: Write `src/config.rs`**

```rust
#[derive(Clone)]
pub struct Config {
    pub database_url: String,
    pub port: u16,
}

impl Config {
    pub fn from_env() -> Self {
        dotenvy::dotenv().ok();
        Self {
            database_url: std::env::var("DATABASE_URL")
                .expect("DATABASE_URL must be set"),
            port: std::env::var("PORT")
                .unwrap_or_else(|_| "8080".to_string())
                .parse()
                .expect("PORT must be a number"),
        }
    }
}
```

- [ ] **Step 6: Write `src/db.rs`**

```rust
use sqlx::postgres::{PgPool, PgPoolOptions};

pub async fn connect_pool(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(5)
        .connect(database_url)
        .await
}
```

- [ ] **Step 7: Write `src/state.rs`**

```rust
use sqlx::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub db: PgPool,
}
```

- [ ] **Step 8: Write `src/routes/health.rs`**

```rust
use axum::{routing::get, Router, Json};
use serde_json::json;
use crate::{state::AppState, error::AppError};

pub fn router() -> Router<AppState> {
    Router::new().route("/health", get(health))
}

async fn health(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> Result<Json<serde_json::Value>, AppError> {
    sqlx::query("SELECT 1").execute(&state.db).await?;
    Ok(Json(json!({ "status": "ok" })))
}
```

- [ ] **Step 9: Write `src/routes/mod.rs`**

```rust
pub mod health;
```

- [ ] **Step 10: Write the failing test `tests/health.rs`**

```rust
use axum::{body::Body, http::{Request, StatusCode}};
use tower::ServiceExt;

#[sqlx::test]
async fn health_returns_ok(pool: sqlx::PgPool) {
    let state = nami_engine::state::AppState { db: pool };
    let app = nami_engine::routes::health::router().with_state(state);

    let response = app
        .oneshot(Request::builder().uri("/health").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}
```

- [ ] **Step 11: Make the crate testable as a library**

Add `nami-engine/src/lib.rs` re-exporting the modules used by tests:

```rust
pub mod config;
pub mod db;
pub mod error;
pub mod state;
pub mod routes;
```

Add `dev-dependencies` needed by the test:

```bash
cargo add tower --dev
cargo add sqlx --dev --features runtime-tokio,postgres,uuid,chrono,migrate,test-attr
```

- [ ] **Step 12: Run the test to verify it fails**

Run: `docker compose up -d postgres && cargo test --test health`
Expected: FAIL (modules/crate layout not wired yet, or compile error) — confirms the test exercises real code once fixed.

- [ ] **Step 13: Write `src/main.rs`**

```rust
use nami_engine::{config::Config, db, state::AppState, routes};
use axum::Router;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let config = Config::from_env();
    let pool = db::connect_pool(&config.database_url)
        .await
        .expect("failed to connect to database");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("failed to run migrations");

    let state = AppState { db: pool };
    let app = Router::new()
        .merge(routes::health::router())
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", config.port))
        .await
        .unwrap();
    tracing::info!("listening on port {}", config.port);
    axum::serve(listener, app).await.unwrap();
}
```

- [ ] **Step 14: Run the test to verify it passes**

Run: `cargo test --test health`
Expected: PASS

- [ ] **Step 15: Manual smoke check**

Run: `docker compose up -d postgres mailpit && cargo run`, then in another shell `curl localhost:8080/health`
Expected: `{"status":"ok"}`

- [ ] **Step 16: Commit**

```bash
git add nami-engine/Cargo.toml nami-engine/Cargo.lock nami-engine/.env.example \
  nami-engine/docker-compose.yaml nami-engine/src nami-engine/tests nami-engine/migrations/.gitkeep
git commit -m "feat(engine): bootstrap axum service with health check"
```

---

### Task 2: Database schema migrations

**Files:**
- Create: `nami-engine/migrations/0001_create_users.sql`
- Create: `nami-engine/migrations/0002_create_oauth_identities.sql`
- Create: `nami-engine/migrations/0003_create_projects.sql`
- Create: `nami-engine/migrations/0004_create_project_members.sql`
- Create: `nami-engine/migrations/0005_create_tokens.sql`
- Test: `nami-engine/tests/migrations.rs`

**Interfaces:**
- Consumes: `sqlx::migrate!("./migrations")` wiring from Task 1's `main.rs` (tests apply migrations via `#[sqlx::test(migrations = "./migrations")]`).
- Produces: tables `users`, `oauth_identities`, `projects`, `project_members`, `email_verification_tokens`, `password_reset_tokens`, consumed by every later task's models.

- [ ] **Step 1: Write `migrations/0001_create_users.sql`**

```sql
create extension if not exists pgcrypto;

create table users (
    id uuid primary key default gen_random_uuid(),
    email text not null unique,
    password_hash text,
    email_verified_at timestamptz,
    global_role text not null default 'member'
        check (global_role in ('admin', 'member', 'viewer')),
    totp_secret text,
    totp_enabled boolean not null default false,
    created_at timestamptz not null default now()
);
```

- [ ] **Step 2: Write `migrations/0002_create_oauth_identities.sql`**

```sql
create table oauth_identities (
    id uuid primary key default gen_random_uuid(),
    user_id uuid not null references users(id) on delete cascade,
    provider text not null check (provider in ('google', 'github')),
    provider_user_id text not null,
    created_at timestamptz not null default now(),
    unique (provider, provider_user_id)
);
```

- [ ] **Step 3: Write `migrations/0003_create_projects.sql`**

```sql
create table projects (
    id uuid primary key default gen_random_uuid(),
    name text not null,
    slug text not null unique,
    created_at timestamptz not null default now()
);
```

- [ ] **Step 4: Write `migrations/0004_create_project_members.sql`**

```sql
create table project_members (
    project_id uuid not null references projects(id) on delete cascade,
    user_id uuid not null references users(id) on delete cascade,
    project_role text not null check (project_role in ('admin', 'member', 'viewer')),
    primary key (project_id, user_id)
);
```

- [ ] **Step 5: Write `migrations/0005_create_tokens.sql`**

```sql
create table email_verification_tokens (
    id uuid primary key default gen_random_uuid(),
    user_id uuid not null references users(id) on delete cascade,
    token_hash text not null,
    expires_at timestamptz not null,
    consumed_at timestamptz
);

create table password_reset_tokens (
    id uuid primary key default gen_random_uuid(),
    user_id uuid not null references users(id) on delete cascade,
    token_hash text not null,
    expires_at timestamptz not null,
    consumed_at timestamptz
);
```

- [ ] **Step 6: Write the failing test `tests/migrations.rs`**

```rust
#[sqlx::test]
async fn all_tables_exist(pool: sqlx::PgPool) {
    let rows: Vec<(String,)> = sqlx::query_as(
        "select table_name from information_schema.tables where table_schema = 'public'",
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    let names: Vec<String> = rows.into_iter().map(|(n,)| n).collect();
    for expected in [
        "users",
        "oauth_identities",
        "projects",
        "project_members",
        "email_verification_tokens",
        "password_reset_tokens",
    ] {
        assert!(names.contains(&expected.to_string()), "missing table {expected}");
    }
}
```

- [ ] **Step 7: Run test to verify it fails**

Run: `cargo test --test migrations`
Expected: FAIL (tables don't exist before migrations are added — if `sqlx::test` auto-applies `./migrations` and finds nothing, the assertion fails on missing table names)

- [ ] **Step 8: Run test to verify it passes**

With the migration files from Steps 1-5 in place:
Run: `cargo test --test migrations`
Expected: PASS

- [ ] **Step 9: Commit**

```bash
git add nami-engine/migrations nami-engine/tests/migrations.rs
git commit -m "feat(engine): add auth/RBAC schema migrations"
```

---

### Task 3: Password hashing

**Files:**
- Create: `nami-engine/src/auth/mod.rs`
- Create: `nami-engine/src/auth/password.rs`

**Interfaces:**
- Produces: `auth::password::hash_password(password: &str) -> Result<String, AppError>`, `auth::password::verify_password(password: &str, hash: &str) -> Result<bool, AppError>`.

- [ ] **Step 1: Add dependency**

```bash
cd nami-engine && cargo add argon2
```

- [ ] **Step 2: Write the failing tests in `src/auth/password.rs`**

```rust
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use argon2::password_hash::{SaltString, rand_core::OsRng};
use crate::error::AppError;

pub fn hash_password(password: &str) -> Result<String, AppError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| AppError::Validation("could not hash password".into()))
}

pub fn verify_password(password: &str, hash: &str) -> Result<bool, AppError> {
    let parsed = PasswordHash::new(hash)
        .map_err(|_| AppError::Validation("invalid password hash".into()))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_succeeds() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(verify_password("correct horse battery staple", &hash).unwrap());
    }

    #[test]
    fn wrong_password_fails() {
        let hash = hash_password("correct horse battery staple").unwrap();
        assert!(!verify_password("wrong password", &hash).unwrap());
    }
}
```

- [ ] **Step 3: Write `src/auth/mod.rs`**

```rust
pub mod password;
```

Add `pub mod auth;` to `src/lib.rs`.

- [ ] **Step 4: Run tests to verify they fail, then pass**

Run: `cargo test --lib auth::password`
Expected: compiles and passes once the code above is in place (write the test function bodies first and confirm they fail against a stubbed `todo!()` implementation if you want the red step; given the implementation is trivial, red can be a compile error from a `todo!()` body before filling in the real one).

- [ ] **Step 5: Commit**

```bash
git add nami-engine/src/auth nami-engine/src/lib.rs nami-engine/Cargo.toml nami-engine/Cargo.lock
git commit -m "feat(engine): add argon2 password hashing"
```

---

### Task 4: Email sending

**Files:**
- Create: `nami-engine/src/email.rs`
- Modify: `nami-engine/src/config.rs`
- Test: `nami-engine/tests/email.rs`

**Interfaces:**
- Consumes: `Config` from Task 1.
- Produces: `Mailer::new(smtp_host: &str, smtp_port: u16) -> Mailer`, `async fn Mailer::send(&self, to: &str, subject: &str, body: String) -> Result<(), AppError>`.

- [ ] **Step 1: Add dependency**

```bash
cd nami-engine && cargo add lettre --features tokio1
cargo add reqwest --dev --features json
```

- [ ] **Step 2: Add SMTP fields to `Config`**

Modify `nami-engine/src/config.rs`:

```rust
pub struct Config {
    pub database_url: String,
    pub port: u16,
    pub smtp_host: String,
    pub smtp_port: u16,
}
```

```rust
smtp_host: std::env::var("SMTP_HOST").unwrap_or_else(|_| "localhost".to_string()),
smtp_port: std::env::var("SMTP_PORT")
    .unwrap_or_else(|_| "1025".to_string())
    .parse()
    .expect("SMTP_PORT must be a number"),
```

Add matching lines to `nami-engine/.env.example`:

```
SMTP_HOST=localhost
SMTP_PORT=1025
```

- [ ] **Step 3: Write `src/email.rs`**

```rust
use lettre::{AsyncTransport, AsyncSmtpTransport, Tokio1Executor, Message};
use crate::error::AppError;

#[derive(Clone)]
pub struct Mailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
}

impl Mailer {
    pub fn new(smtp_host: &str, smtp_port: u16) -> Self {
        let transport = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(smtp_host)
            .port(smtp_port)
            .build();
        Self { transport }
    }

    pub async fn send(&self, to: &str, subject: &str, body: String) -> Result<(), AppError> {
        let email = Message::builder()
            .from("Nami <noreply@nami.local>".parse().unwrap())
            .to(to.parse().map_err(|_| AppError::Validation("invalid recipient".into()))?)
            .subject(subject)
            .body(body)
            .map_err(|_| AppError::Validation("could not build email".into()))?;

        self.transport
            .send(email)
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "failed to send email");
                AppError::Validation("could not send email".into())
            })?;
        Ok(())
    }
}
```

Add `pub mod email;` to `src/lib.rs`. Add `mailer: email::Mailer` to `AppState` in `src/state.rs`, and construct it in `main.rs` with `email::Mailer::new(&config.smtp_host, config.smtp_port)`.

- [ ] **Step 4: Write the failing test `tests/email.rs`**

```rust
use nami_engine::email::Mailer;
use serde_json::Value;

#[tokio::test]
async fn sent_email_is_captured_by_mailpit() {
    // Requires `docker compose up -d mailpit` to be running first.
    reqwest::Client::new()
        .delete("http://localhost:8025/api/v1/messages")
        .send()
        .await
        .unwrap();

    let mailer = Mailer::new("localhost", 1025);
    mailer
        .send("someone@example.com", "Verify your email", "click here".to_string())
        .await
        .unwrap();

    let body: Value = reqwest::get("http://localhost:8025/api/v1/messages")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["Subject"], "Verify your email");
}
```

- [ ] **Step 5: Run test to verify it fails**

Run: `docker compose up -d mailpit && cargo test --test email`
Expected: FAIL before `email.rs` exists / compiles.

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test --test email`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add nami-engine/src/email.rs nami-engine/src/config.rs nami-engine/src/state.rs \
  nami-engine/src/main.rs nami-engine/src/lib.rs nami-engine/.env.example \
  nami-engine/tests/email.rs nami-engine/Cargo.toml nami-engine/Cargo.lock
git commit -m "feat(engine): send email via lettre to Mailpit"
```

---

### Task 5: Opaque token generation and hashing

**Files:**
- Create: `nami-engine/src/auth/token.rs`

**Interfaces:**
- Produces: `auth::token::new_opaque_token() -> (String /* raw */, String /* sha256 hex hash */)`, `auth::token::hash_token(raw: &str) -> String`.

- [ ] **Step 1: Add dependencies**

```bash
cd nami-engine && cargo add sha2 rand hex
```

- [ ] **Step 2: Write the failing tests in `src/auth/token.rs`**

```rust
use rand::RngCore;
use sha2::{Digest, Sha256};

pub fn hash_token(raw: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(raw.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub fn new_opaque_token() -> (String, String) {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    let raw = hex::encode(bytes);
    let hash = hash_token(&raw);
    (raw, hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic() {
        assert_eq!(hash_token("abc"), hash_token("abc"));
    }

    #[test]
    fn different_tokens_hash_differently() {
        let (raw1, hash1) = new_opaque_token();
        let (raw2, hash2) = new_opaque_token();
        assert_ne!(raw1, raw2);
        assert_ne!(hash1, hash2);
    }
}
```

- [ ] **Step 3: Register the module**

Add `pub mod token;` to `src/auth/mod.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test --lib auth::token`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add nami-engine/src/auth/token.rs nami-engine/src/auth/mod.rs nami-engine/Cargo.toml nami-engine/Cargo.lock
git commit -m "feat(engine): add opaque token generation for verification/reset"
```

---

### Task 6: Registration and email verification

**Files:**
- Create: `nami-engine/src/models/mod.rs`
- Create: `nami-engine/src/models/user.rs`
- Create: `nami-engine/src/models/token.rs`
- Create: `nami-engine/src/routes/auth.rs`
- Modify: `nami-engine/src/routes/mod.rs`
- Modify: `nami-engine/src/main.rs`
- Test: `nami-engine/tests/register.rs`

**Interfaces:**
- Consumes: `auth::password::hash_password`, `auth::token::new_opaque_token`, `email::Mailer::send`, `AppState { db, mailer }`.
- Produces: `models::user::User { id: Uuid, email: String, password_hash: Option<String>, email_verified_at: Option<DateTime<Utc>>, global_role: String, totp_secret: Option<String>, totp_enabled: bool }`, `models::user::find_by_email`, `models::user::insert_user`, `models::user::mark_email_verified`, `models::token::insert_verification_token`, `models::token::consume_valid_verification_token`, `routes::auth::router() -> Router<AppState>` mounted at `POST /auth/register`, `GET /auth/verify-email`.

- [ ] **Step 1: Write `src/models/user.rs`**

```rust
use sqlx::PgPool;
use uuid::Uuid;
use chrono::{DateTime, Utc};

#[derive(Debug, sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub password_hash: Option<String>,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub global_role: String,
    pub totp_secret: Option<String>,
    pub totp_enabled: bool,
}

pub async fn find_by_email(pool: &PgPool, email: &str) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as::<_, User>("select * from users where email = $1")
        .bind(email)
        .fetch_optional(pool)
        .await
}

pub async fn insert_user(
    pool: &PgPool,
    email: &str,
    password_hash: &str,
) -> Result<User, sqlx::Error> {
    sqlx::query_as::<_, User>(
        "insert into users (email, password_hash) values ($1, $2) returning *",
    )
    .bind(email)
    .bind(password_hash)
    .fetch_one(pool)
    .await
}

pub async fn mark_email_verified(pool: &PgPool, user_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("update users set email_verified_at = now() where id = $1")
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}
```

- [ ] **Step 2: Write `src/models/token.rs`**

```rust
use sqlx::PgPool;
use uuid::Uuid;
use chrono::{Duration, Utc};

pub async fn insert_verification_token(
    pool: &PgPool,
    user_id: Uuid,
    token_hash: &str,
) -> Result<(), sqlx::Error> {
    let expires_at = Utc::now() + Duration::hours(24);
    sqlx::query(
        "insert into email_verification_tokens (user_id, token_hash, expires_at) values ($1, $2, $3)",
    )
    .bind(user_id)
    .bind(token_hash)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// Returns the user_id if the token is valid and unconsumed, and marks it consumed.
pub async fn consume_valid_verification_token(
    pool: &PgPool,
    token_hash: &str,
) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(
        "select id, user_id from email_verification_tokens
         where token_hash = $1 and expires_at > now() and consumed_at is null",
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await?;

    if let Some((token_id, user_id)) = row {
        sqlx::query("update email_verification_tokens set consumed_at = now() where id = $1")
            .bind(token_id)
            .execute(pool)
            .await?;
        Ok(Some(user_id))
    } else {
        Ok(None)
    }
}
```

- [ ] **Step 3: Write `src/models/mod.rs`**

```rust
pub mod user;
pub mod token;
```

Add `pub mod models;` to `src/lib.rs`.

- [ ] **Step 4: Write `src/routes/auth.rs`**

```rust
use axum::{routing::{get, post}, Router, Json, extract::{State, Query}};
use serde::Deserialize;
use serde_json::{json, Value};
use crate::{state::AppState, error::AppError, auth, models};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/register", post(register))
        .route("/auth/verify-email", get(verify_email))
}

#[derive(Deserialize)]
struct RegisterRequest {
    email: String,
    password: String,
}

async fn register(
    State(state): State<AppState>,
    Json(payload): Json<RegisterRequest>,
) -> Result<Json<Value>, AppError> {
    let generic_response = Json(json!({
        "message": "if this email is new, a verification link has been sent"
    }));

    if models::user::find_by_email(&state.db, &payload.email).await?.is_some() {
        return Ok(generic_response);
    }

    let password_hash = auth::password::hash_password(&payload.password)?;
    let user = models::user::insert_user(&state.db, &payload.email, &password_hash).await?;

    let (raw_token, token_hash) = auth::token::new_opaque_token();
    models::token::insert_verification_token(&state.db, user.id, &token_hash).await?;

    state
        .mailer
        .send(
            &user.email,
            "Verify your Nami account",
            format!("Verify your email: /auth/verify-email?token={raw_token}"),
        )
        .await?;

    Ok(generic_response)
}

#[derive(Deserialize)]
struct VerifyEmailQuery {
    token: String,
}

async fn verify_email(
    State(state): State<AppState>,
    Query(query): Query<VerifyEmailQuery>,
) -> Result<Json<Value>, AppError> {
    let token_hash = auth::token::hash_token(&query.token);
    let user_id = models::token::consume_valid_verification_token(&state.db, &token_hash)
        .await?
        .ok_or_else(|| AppError::Validation("invalid or expired token".into()))?;

    models::user::mark_email_verified(&state.db, user_id).await?;
    Ok(Json(json!({ "message": "email verified" })))
}
```

Add `pub mod auth;` to `src/routes/mod.rs`, and `.merge(routes::auth::router())` to the `Router` built in `main.rs`.

- [ ] **Step 5: Write the failing test `tests/register.rs`**

```rust
use axum::{body::Body, http::{Request, StatusCode}};
use tower::ServiceExt;
use nami_engine::{state::AppState, email::Mailer, routes};
use serde_json::json;

fn test_app(pool: sqlx::PgPool) -> axum::Router {
    let state = AppState { db: pool, mailer: Mailer::new("localhost", 1025) };
    axum::Router::new()
        .merge(routes::auth::router())
        .with_state(state)
}

#[sqlx::test]
async fn register_creates_unverified_user(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let body = json!({ "email": "new@example.com", "password": "password123" }).to_string();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/register")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let user: (uuid::Uuid, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("select id, email_verified_at from users where email = $1")
            .bind("new@example.com")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(user.1.is_none());
}

#[sqlx::test]
async fn registering_twice_does_not_duplicate_or_change_response(pool: sqlx::PgPool) {
    let app1 = test_app(pool.clone());
    let app2 = test_app(pool.clone());
    let body = json!({ "email": "dup@example.com", "password": "password123" }).to_string();

    let make_request = || {
        Request::builder()
            .method("POST")
            .uri("/auth/register")
            .header("content-type", "application/json")
            .body(Body::from(body.clone()))
            .unwrap()
    };

    let r1 = app1.oneshot(make_request()).await.unwrap();
    assert_eq!(r1.status(), StatusCode::OK);

    let r2 = app2.oneshot(make_request()).await.unwrap();
    assert_eq!(r2.status(), StatusCode::OK);

    let count: (i64,) = sqlx::query_as("select count(*) from users where email = $1")
        .bind("dup@example.com")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);
}
```

- [ ] **Step 6: Run tests to verify they fail, then pass**

Run: `docker compose up -d postgres mailpit && cargo test --test register`
Expected: FAIL before `routes/auth.rs` exists, PASS after.

- [ ] **Step 7: Add a verify-email test**

Append to `tests/register.rs`:

```rust
#[sqlx::test]
async fn verify_email_rejects_invalid_token(pool: sqlx::PgPool) {
    let app = test_app(pool.clone());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/auth/verify-email?token=not-a-real-token")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
```

Run: `cargo test --test register`
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add nami-engine/src/models nami-engine/src/routes/auth.rs nami-engine/src/routes/mod.rs \
  nami-engine/src/main.rs nami-engine/src/lib.rs nami-engine/tests/register.rs
git commit -m "feat(engine): add registration and email verification"
```

---

### Task 7: Login, logout, sessions, and `/auth/me`

**Files:**
- Modify: `nami-engine/src/routes/auth.rs`
- Modify: `nami-engine/src/main.rs`
- Modify: `nami-engine/src/state.rs`
- Test: `nami-engine/tests/login.rs`

**Interfaces:**
- Consumes: `auth::password::verify_password`, `models::user::find_by_email`.
- Produces: `POST /auth/login`, `POST /auth/logout`, `GET /auth/me`; session keys `"user_id": Uuid` and `"auth_at": DateTime<Utc>` written via `tower_sessions::Session`.

- [ ] **Step 1: Add dependencies**

```bash
cd nami-engine && cargo add tower-sessions
cargo add tower-sessions-sqlx-store --features postgres
cargo add time
```

- [ ] **Step 2: Wire the session layer in `main.rs`**

```rust
use tower_sessions::{SessionManagerLayer, Expiry};
use tower_sessions_sqlx_store::PostgresStore;
use time::Duration as TimeDuration;

// after connecting `pool` and running migrations:
let session_store = PostgresStore::new(pool.clone());
session_store.migrate().await.expect("failed to migrate session store");

let session_layer = SessionManagerLayer::new(session_store)
    .with_secure(false) // set true once served over HTTPS in production
    .with_expiry(Expiry::OnInactivity(TimeDuration::hours(24)));
```

Apply `.layer(session_layer)` to the `Router` after `.merge(routes::auth::router())`.

- [ ] **Step 3: Extend `src/routes/auth.rs` with login/logout/me**

```rust
use tower_sessions::Session;

// add to router():
.route("/auth/login", post(login))
.route("/auth/logout", post(logout))
.route("/auth/me", get(me))
```

```rust
#[derive(Deserialize)]
struct LoginRequest {
    email: String,
    password: String,
}

async fn login(
    State(state): State<AppState>,
    session: Session,
    Json(payload): Json<LoginRequest>,
) -> Result<Json<Value>, AppError> {
    let user = models::user::find_by_email(&state.db, &payload.email)
        .await?
        .ok_or(AppError::Unauthorized)?;

    let password_hash = user.password_hash.as_deref().ok_or(AppError::Unauthorized)?;
    if !auth::password::verify_password(&payload.password, password_hash)? {
        return Err(AppError::Unauthorized);
    }

    if user.totp_enabled {
        session
            .insert("pending_user_id", user.id)
            .await
            .map_err(|_| AppError::Validation("session error".into()))?;
        return Ok(Json(json!({ "requires_totp": true })));
    }

    session
        .insert("user_id", user.id)
        .await
        .map_err(|_| AppError::Validation("session error".into()))?;
    session
        .insert("auth_at", chrono::Utc::now())
        .await
        .map_err(|_| AppError::Validation("session error".into()))?;

    Ok(Json(json!({ "authenticated": true })))
}

async fn logout(session: Session) -> Result<Json<Value>, AppError> {
    session
        .delete()
        .await
        .map_err(|_| AppError::Validation("session error".into()))?;
    Ok(Json(json!({ "message": "logged out" })))
}

async fn me(
    State(state): State<AppState>,
    session: Session,
) -> Result<Json<Value>, AppError> {
    let user_id: uuid::Uuid = session
        .get("user_id")
        .await
        .map_err(|_| AppError::Validation("session error".into()))?
        .ok_or(AppError::Unauthorized)?;

    let auth_at: chrono::DateTime<chrono::Utc> = session
        .get("auth_at")
        .await
        .map_err(|_| AppError::Validation("session error".into()))?
        .ok_or(AppError::Unauthorized)?;

    let user: models::user::User = sqlx::query_as("select * from users where id = $1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::Unauthorized)?;

    // Task 9 adds `password_changed_at`; until then this check is a no-op.
    let _ = auth_at;

    Ok(Json(json!({ "id": user.id, "email": user.email, "global_role": user.global_role })))
}
```

- [ ] **Step 4: Write the failing test `tests/login.rs`**

```rust
use axum::{body::Body, http::{Request, StatusCode, header}};
use tower::ServiceExt;
use nami_engine::{state::AppState, email::Mailer, routes};
use serde_json::json;

fn test_app(pool: sqlx::PgPool) -> axum::Router {
    let state = AppState { db: pool, mailer: Mailer::new("localhost", 1025) };
    axum::Router::new()
        .merge(routes::auth::router())
        .layer(tower_sessions::SessionManagerLayer::new(
            tower_sessions::MemoryStore::default(),
        ))
        .with_state(state)
}

#[sqlx::test]
async fn login_then_me_succeeds(pool: sqlx::PgPool) {
    let password_hash = nami_engine::auth::password::hash_password("password123").unwrap();
    sqlx::query("insert into users (email, password_hash, email_verified_at) values ($1, $2, now())")
        .bind("user@example.com")
        .bind(&password_hash)
        .execute(&pool)
        .await
        .unwrap();

    let app = test_app(pool.clone());
    let login_res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(json!({"email": "user@example.com", "password": "password123"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login_res.status(), StatusCode::OK);

    let cookie = login_res.headers().get(header::SET_COOKIE).unwrap().clone();

    let me_res = app
        .oneshot(
            Request::builder()
                .uri("/auth/me")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(me_res.status(), StatusCode::OK);
}

#[sqlx::test]
async fn login_rejects_oauth_only_account(pool: sqlx::PgPool) {
    sqlx::query("insert into users (email, password_hash, email_verified_at) values ($1, null, now())")
        .bind("oauth-only@example.com")
        .execute(&pool)
        .await
        .unwrap();

    let app = test_app(pool.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(json!({"email": "oauth-only@example.com", "password": "anything"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}
```

- [ ] **Step 5: Add `tower_sessions`'s in-memory store for tests**

```bash
cargo add tower-sessions --dev --features memory-store
```

- [ ] **Step 6: Run tests to verify they fail, then pass**

Run: `cargo test --test login`
Expected: FAIL before login/me exist, PASS after.

- [ ] **Step 7: Commit**

```bash
git add nami-engine/src/routes/auth.rs nami-engine/src/main.rs nami-engine/src/state.rs \
  nami-engine/tests/login.rs nami-engine/Cargo.toml nami-engine/Cargo.lock
git commit -m "feat(engine): add login, logout, and session-backed /auth/me"
```

---

### Task 8: TOTP 2FA

**Files:**
- Modify: `nami-engine/src/routes/auth.rs`
- Create: `nami-engine/src/auth/totp.rs`
- Test: `nami-engine/tests/totp.rs`

**Interfaces:**
- Consumes: `models::user::User.totp_secret`, `User.totp_enabled`, session keys from Task 7.
- Produces: `auth::totp::generate_secret() -> String`, `auth::totp::provisioning_uri(secret: &str, email: &str) -> String`, `auth::totp::verify_code(secret: &str, code: &str) -> bool`; routes `POST /auth/totp/setup`, `POST /auth/totp/enable`, `POST /auth/totp/verify`.

- [ ] **Step 1: Add dependency**

```bash
cd nami-engine && cargo add totp-rs --features gen_secret
```

- [ ] **Step 2: Write `src/auth/totp.rs`**

```rust
use totp_rs::{TOTP, Algorithm, Secret};

fn totp_for(secret: &str, email: &str) -> TOTP {
    TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        Secret::Encoded(secret.to_string()).to_bytes().unwrap(),
        Some("Nami".to_string()),
        email.to_string(),
    )
    .unwrap()
}

pub fn generate_secret() -> String {
    Secret::generate_secret().to_encoded().to_string()
}

pub fn provisioning_uri(secret: &str, email: &str) -> String {
    totp_for(secret, email).get_url()
}

pub fn verify_code(secret: &str, email: &str, code: &str) -> bool {
    totp_for(secret, email).check_current(code).unwrap_or(false)
}
```

Add `pub mod totp;` to `src/auth/mod.rs`.

- [ ] **Step 3: Add setup/enable/verify routes to `src/routes/auth.rs`**

```rust
// add to router():
.route("/auth/totp/setup", post(totp_setup))
.route("/auth/totp/enable", post(totp_enable))
.route("/auth/totp/verify", post(totp_verify))
```

```rust
async fn current_user_id(session: &Session) -> Result<uuid::Uuid, AppError> {
    session
        .get("user_id")
        .await
        .map_err(|_| AppError::Validation("session error".into()))?
        .ok_or(AppError::Unauthorized)
}

async fn totp_setup(
    State(state): State<AppState>,
    session: Session,
) -> Result<Json<Value>, AppError> {
    let user_id = current_user_id(&session).await?;
    let user: models::user::User = sqlx::query_as("select * from users where id = $1")
        .bind(user_id)
        .fetch_one(&state.db)
        .await?;

    let secret = auth::totp::generate_secret();
    sqlx::query("update users set totp_secret = $1, totp_enabled = false where id = $2")
        .bind(&secret)
        .bind(user_id)
        .execute(&state.db)
        .await?;

    Ok(Json(json!({ "provisioning_uri": auth::totp::provisioning_uri(&secret, &user.email) })))
}

#[derive(Deserialize)]
struct TotpCodeRequest {
    code: String,
}

async fn totp_enable(
    State(state): State<AppState>,
    session: Session,
    Json(payload): Json<TotpCodeRequest>,
) -> Result<Json<Value>, AppError> {
    let user_id = current_user_id(&session).await?;
    let user: models::user::User = sqlx::query_as("select * from users where id = $1")
        .bind(user_id)
        .fetch_one(&state.db)
        .await?;
    let secret = user.totp_secret.ok_or_else(|| AppError::Validation("run setup first".into()))?;

    if !auth::totp::verify_code(&secret, &user.email, &payload.code) {
        return Err(AppError::Validation("invalid code".into()));
    }

    sqlx::query("update users set totp_enabled = true where id = $1")
        .bind(user_id)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({ "enabled": true })))
}

async fn totp_verify(
    State(state): State<AppState>,
    session: Session,
    Json(payload): Json<TotpCodeRequest>,
) -> Result<Json<Value>, AppError> {
    let pending_user_id: uuid::Uuid = session
        .get("pending_user_id")
        .await
        .map_err(|_| AppError::Validation("session error".into()))?
        .ok_or(AppError::Unauthorized)?;

    let user: models::user::User = sqlx::query_as("select * from users where id = $1")
        .bind(pending_user_id)
        .fetch_one(&state.db)
        .await?;
    let secret = user.totp_secret.ok_or(AppError::Unauthorized)?;

    if !auth::totp::verify_code(&secret, &user.email, &payload.code) {
        return Err(AppError::Unauthorized);
    }

    session.remove::<uuid::Uuid>("pending_user_id").await.ok();
    session
        .insert("user_id", user.id)
        .await
        .map_err(|_| AppError::Validation("session error".into()))?;
    session
        .insert("auth_at", chrono::Utc::now())
        .await
        .map_err(|_| AppError::Validation("session error".into()))?;

    Ok(Json(json!({ "authenticated": true })))
}
```

- [ ] **Step 4: Write the failing test `tests/totp.rs`**

Covers Review Focus item: a login with TOTP enabled but a missing/wrong code never produces a working session.

```rust
use axum::{body::Body, http::{Request, StatusCode, header}};
use tower::ServiceExt;
use nami_engine::{state::AppState, email::Mailer, routes, auth};
use serde_json::json;

fn test_app(pool: sqlx::PgPool) -> axum::Router {
    let state = AppState { db: pool, mailer: Mailer::new("localhost", 1025) };
    axum::Router::new()
        .merge(routes::auth::router())
        .layer(tower_sessions::SessionManagerLayer::new(
            tower_sessions::MemoryStore::default(),
        ))
        .with_state(state)
}

#[sqlx::test]
async fn wrong_totp_code_never_creates_working_session(pool: sqlx::PgPool) {
    let password_hash = auth::password::hash_password("password123").unwrap();
    let secret = auth::totp::generate_secret();
    sqlx::query(
        "insert into users (email, password_hash, email_verified_at, totp_secret, totp_enabled)
         values ($1, $2, now(), $3, true)",
    )
    .bind("2fa@example.com")
    .bind(&password_hash)
    .bind(&secret)
    .execute(&pool)
    .await
    .unwrap();

    let app = test_app(pool.clone());
    let login_res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(json!({"email": "2fa@example.com", "password": "password123"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login_res.status(), StatusCode::OK);
    let cookie = login_res.headers().get(header::SET_COOKIE).unwrap().clone();

    let verify_res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/totp/verify")
                .header("content-type", "application/json")
                .header(header::COOKIE, cookie.clone())
                .body(Body::from(json!({"code": "000000"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(verify_res.status(), StatusCode::UNAUTHORIZED);

    let me_res = app
        .oneshot(
            Request::builder()
                .uri("/auth/me")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(me_res.status(), StatusCode::UNAUTHORIZED);
}
```

- [ ] **Step 5: Run test to verify it fails, then passes**

Run: `cargo test --test totp`
Expected: FAIL before TOTP routes exist, PASS after.

- [ ] **Step 6: Commit**

```bash
git add nami-engine/src/auth/totp.rs nami-engine/src/auth/mod.rs nami-engine/src/routes/auth.rs \
  nami-engine/tests/totp.rs nami-engine/Cargo.toml nami-engine/Cargo.lock
git commit -m "feat(engine): add TOTP-based two-factor authentication"
```

---

### Task 9: Password reset and session invalidation

**Files:**
- Create: `nami-engine/migrations/0006_add_password_changed_at.sql`
- Modify: `nami-engine/src/models/user.rs`
- Modify: `nami-engine/src/models/token.rs`
- Modify: `nami-engine/src/routes/auth.rs`
- Test: `nami-engine/tests/password_reset.rs`

**Interfaces:**
- Produces: `models::user::update_password(pool, user_id, new_hash) -> Result<(), sqlx::Error>` (also bumps `password_changed_at`), `models::token::insert_reset_token`, `models::token::consume_valid_reset_token`; routes `POST /auth/password/forgot`, `POST /auth/password/reset`.

- [ ] **Step 1: Write `migrations/0006_add_password_changed_at.sql`**

```sql
alter table users add column password_changed_at timestamptz not null default now();
```

- [ ] **Step 2: Add `update_password` to `src/models/user.rs`**

```rust
pub async fn update_password(pool: &PgPool, user_id: Uuid, new_hash: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "update users set password_hash = $1, password_changed_at = now() where id = $2",
    )
    .bind(new_hash)
    .bind(user_id)
    .execute(pool)
    .await?;
    Ok(())
}
```

Add `pub password_changed_at: DateTime<Utc>` to the `User` struct.

- [ ] **Step 3: Add reset-token helpers to `src/models/token.rs`**

```rust
pub async fn insert_reset_token(pool: &PgPool, user_id: Uuid, token_hash: &str) -> Result<(), sqlx::Error> {
    let expires_at = Utc::now() + Duration::hours(1);
    sqlx::query(
        "insert into password_reset_tokens (user_id, token_hash, expires_at) values ($1, $2, $3)",
    )
    .bind(user_id)
    .bind(token_hash)
    .bind(expires_at)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn consume_valid_reset_token(pool: &PgPool, token_hash: &str) -> Result<Option<Uuid>, sqlx::Error> {
    let row: Option<(Uuid, Uuid)> = sqlx::query_as(
        "select id, user_id from password_reset_tokens
         where token_hash = $1 and expires_at > now() and consumed_at is null",
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await?;

    if let Some((token_id, user_id)) = row {
        sqlx::query("update password_reset_tokens set consumed_at = now() where id = $1")
            .bind(token_id)
            .execute(pool)
            .await?;
        Ok(Some(user_id))
    } else {
        Ok(None)
    }
}
```

- [ ] **Step 4: Add forgot/reset routes to `src/routes/auth.rs`**

```rust
// add to router():
.route("/auth/password/forgot", post(forgot_password))
.route("/auth/password/reset", post(reset_password))
```

```rust
#[derive(Deserialize)]
struct ForgotPasswordRequest {
    email: String,
}

async fn forgot_password(
    State(state): State<AppState>,
    Json(payload): Json<ForgotPasswordRequest>,
) -> Result<Json<Value>, AppError> {
    let generic_response = Json(json!({ "message": "if this email exists, a reset link was sent" }));

    let Some(user) = models::user::find_by_email(&state.db, &payload.email).await? else {
        return Ok(generic_response);
    };

    let (raw_token, token_hash) = auth::token::new_opaque_token();
    models::token::insert_reset_token(&state.db, user.id, &token_hash).await?;
    state
        .mailer
        .send(&user.email, "Reset your Nami password", format!("Reset: /auth/password/reset?token={raw_token}"))
        .await?;

    Ok(generic_response)
}

#[derive(Deserialize)]
struct ResetPasswordRequest {
    token: String,
    new_password: String,
}

async fn reset_password(
    State(state): State<AppState>,
    Json(payload): Json<ResetPasswordRequest>,
) -> Result<Json<Value>, AppError> {
    let token_hash = auth::token::hash_token(&payload.token);
    let user_id = models::token::consume_valid_reset_token(&state.db, &token_hash)
        .await?
        .ok_or_else(|| AppError::Validation("invalid or expired token".into()))?;

    let new_hash = auth::password::hash_password(&payload.new_password)?;
    models::user::update_password(&state.db, user_id, &new_hash).await?;

    Ok(Json(json!({ "message": "password reset" })))
}
```

- [ ] **Step 5: Make `/auth/me` honor `password_changed_at` (staleness check)**

Modify `me` in `src/routes/auth.rs`: replace the `let _ = auth_at;` placeholder line from Task 7 with:

```rust
if auth_at < user.password_changed_at {
    session.delete().await.ok();
    return Err(AppError::Unauthorized);
}
```

- [ ] **Step 6: Write the failing test `tests/password_reset.rs`**

Covers Review Focus item: an expired or already-consumed reset token must be rejected, and a reset invalidates the old session.

```rust
use axum::{body::Body, http::{Request, StatusCode, header}};
use tower::ServiceExt;
use nami_engine::{state::AppState, email::Mailer, routes, auth};
use serde_json::json;

fn test_app(pool: sqlx::PgPool) -> axum::Router {
    let state = AppState { db: pool, mailer: Mailer::new("localhost", 1025) };
    axum::Router::new()
        .merge(routes::auth::router())
        .layer(tower_sessions::SessionManagerLayer::new(
            tower_sessions::MemoryStore::default(),
        ))
        .with_state(state)
}

#[sqlx::test]
async fn expired_reset_token_is_rejected(pool: sqlx::PgPool) {
    let password_hash = auth::password::hash_password("password123").unwrap();
    let user_id: (uuid::Uuid,) = sqlx::query_as(
        "insert into users (email, password_hash, email_verified_at) values ($1, $2, now()) returning id",
    )
    .bind("reset@example.com")
    .bind(&password_hash)
    .fetch_one(&pool)
    .await
    .unwrap();

    let (raw_token, token_hash) = auth::token::new_opaque_token();
    sqlx::query(
        "insert into password_reset_tokens (user_id, token_hash, expires_at) values ($1, $2, now() - interval '1 hour')",
    )
    .bind(user_id.0)
    .bind(&token_hash)
    .execute(&pool)
    .await
    .unwrap();

    let app = test_app(pool.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/password/reset")
                .header("content-type", "application/json")
                .body(Body::from(json!({"token": raw_token, "new_password": "newpass123"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn reset_invalidates_existing_session(pool: sqlx::PgPool) {
    let password_hash = auth::password::hash_password("password123").unwrap();
    sqlx::query("insert into users (email, password_hash, email_verified_at) values ($1, $2, now())")
        .bind("reset2@example.com")
        .bind(&password_hash)
        .execute(&pool)
        .await
        .unwrap();

    let app = test_app(pool.clone());
    let login_res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(json!({"email": "reset2@example.com", "password": "password123"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let cookie = login_res.headers().get(header::SET_COOKIE).unwrap().clone();

    let user_id: (uuid::Uuid,) = sqlx::query_as("select id from users where email = $1")
        .bind("reset2@example.com")
        .fetch_one(&pool)
        .await
        .unwrap();
    let (raw_token, token_hash) = auth::token::new_opaque_token();
    sqlx::query(
        "insert into password_reset_tokens (user_id, token_hash, expires_at) values ($1, $2, now() + interval '1 hour')",
    )
    .bind(user_id.0)
    .bind(&token_hash)
    .execute(&pool)
    .await
    .unwrap();

    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/password/reset")
                .header("content-type", "application/json")
                .body(Body::from(json!({"token": raw_token, "new_password": "brandnewpass"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    let me_res = app
        .oneshot(
            Request::builder()
                .uri("/auth/me")
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(me_res.status(), StatusCode::UNAUTHORIZED);
}
```

- [ ] **Step 7: Run tests to verify they fail, then pass**

Run: `cargo test --test password_reset`
Expected: FAIL before routes/staleness check exist, PASS after.

- [ ] **Step 8: Commit**

```bash
git add nami-engine/migrations/0006_add_password_changed_at.sql nami-engine/src/models \
  nami-engine/src/routes/auth.rs nami-engine/tests/password_reset.rs
git commit -m "feat(engine): add password reset with session invalidation"
```

---

### Task 10: OAuth login (Google + GitHub)

**Files:**
- Modify: `nami-engine/src/config.rs`
- Create: `nami-engine/src/auth/oauth.rs`
- Modify: `nami-engine/src/models/user.rs`
- Modify: `nami-engine/src/routes/auth.rs`
- Modify: `nami-engine/src/state.rs`
- Test: `nami-engine/tests/oauth.rs`

**Interfaces:**
- Produces: `auth::oauth::Provider { Google, Github }`, `auth::oauth::client(provider, config) -> BasicClient`, `auth::oauth::userinfo_url(provider, config) -> String`, `models::user::find_or_create_oauth_user(pool, provider, provider_user_id, email) -> Result<User, sqlx::Error>`; routes `GET /auth/oauth/:provider/start`, `GET /auth/oauth/:provider/callback`.

- [ ] **Step 1: Add dependencies**

```bash
cd nami-engine && cargo add oauth2
cargo add reqwest --features json
cargo add wiremock --dev
```

- [ ] **Step 2: Extend `Config` with OAuth settings (made URL-overridable for tests)**

```rust
pub struct Config {
    // ...existing fields...
    pub oauth_redirect_base_url: String,
    pub google_client_id: String,
    pub google_client_secret: String,
    pub google_auth_url: String,
    pub google_token_url: String,
    pub google_userinfo_url: String,
    pub github_client_id: String,
    pub github_client_secret: String,
    pub github_auth_url: String,
    pub github_token_url: String,
    pub github_userinfo_url: String,
}
```

Load each with `std::env::var(...).unwrap_or_else(|_| "<default>".to_string())` so tests can override the `*_url` fields to point at a `wiremock` server while leaving real Google/GitHub endpoints as the defaults:

```rust
oauth_redirect_base_url: std::env::var("OAUTH_REDIRECT_BASE_URL")
    .unwrap_or_else(|_| "http://localhost:5173".to_string()),
google_client_id: std::env::var("GOOGLE_CLIENT_ID").unwrap_or_default(),
google_client_secret: std::env::var("GOOGLE_CLIENT_SECRET").unwrap_or_default(),
google_auth_url: std::env::var("GOOGLE_AUTH_URL")
    .unwrap_or_else(|_| "https://accounts.google.com/o/oauth2/v2/auth".to_string()),
google_token_url: std::env::var("GOOGLE_TOKEN_URL")
    .unwrap_or_else(|_| "https://oauth2.googleapis.com/token".to_string()),
google_userinfo_url: std::env::var("GOOGLE_USERINFO_URL")
    .unwrap_or_else(|_| "https://www.googleapis.com/oauth2/v3/userinfo".to_string()),
github_client_id: std::env::var("GITHUB_CLIENT_ID").unwrap_or_default(),
github_client_secret: std::env::var("GITHUB_CLIENT_SECRET").unwrap_or_default(),
github_auth_url: std::env::var("GITHUB_AUTH_URL")
    .unwrap_or_else(|_| "https://github.com/login/oauth/authorize".to_string()),
github_token_url: std::env::var("GITHUB_TOKEN_URL")
    .unwrap_or_else(|_| "https://github.com/login/oauth/access_token".to_string()),
github_userinfo_url: std::env::var("GITHUB_USERINFO_URL")
    .unwrap_or_else(|_| "https://api.github.com/user".to_string()),
```

Add a `Config::from_env_for_test()` constructor next to `from_env()` in `src/config.rs`, identical except it doesn't require `DATABASE_URL`/`GOOGLE_CLIENT_ID`/etc. to be set (uses `unwrap_or_default()` for every field `from_env()` would `.expect()` on), so tests don't need a `.env` file:

```rust
pub fn from_env_for_test() -> Self {
    Self {
        database_url: String::new(), // sqlx::test supplies its own pool directly
        port: 8080,
        smtp_host: "localhost".to_string(),
        smtp_port: 1025,
        ..Self::from_env_defaults_only()
    }
}
```

(Factor the `oauth_redirect_base_url`/provider-URL block above into a private `from_env_defaults_only()` helper shared by both `from_env()` and `from_env_for_test()`, so the two constructors don't duplicate those lines.)

Add matching entries to `.env.example`.

Add `pub config: Config` to `AppState` in `src/state.rs`, and construct it in `main.rs` alongside the existing fields:

```rust
let state = AppState { db: pool, mailer: email::Mailer::new(&config.smtp_host, config.smtp_port), config };
```

(This replaces the `AppState { db: pool }` / two-field construction left by Task 4 in `main.rs`.)

- [ ] **Step 3: Write `src/auth/oauth.rs`**

```rust
use oauth2::{basic::BasicClient, AuthUrl, ClientId, ClientSecret, RedirectUrl, TokenUrl};
use crate::config::Config;

#[derive(Clone, Copy, Debug)]
pub enum Provider {
    Google,
    Github,
}

impl Provider {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "google" => Some(Provider::Google),
            "github" => Some(Provider::Github),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Google => "google",
            Provider::Github => "github",
        }
    }

    pub fn userinfo_url(&self, config: &Config) -> String {
        match self {
            Provider::Google => config.google_userinfo_url.clone(),
            Provider::Github => config.github_userinfo_url.clone(),
        }
    }

    pub fn client(&self, config: &Config) -> BasicClient {
        let (client_id, client_secret, auth_url, token_url) = match self {
            Provider::Google => (
                &config.google_client_id,
                &config.google_client_secret,
                &config.google_auth_url,
                &config.google_token_url,
            ),
            Provider::Github => (
                &config.github_client_id,
                &config.github_client_secret,
                &config.github_auth_url,
                &config.github_token_url,
            ),
        };

        BasicClient::new(
            ClientId::new(client_id.clone()),
            Some(ClientSecret::new(client_secret.clone())),
            AuthUrl::new(auth_url.clone()).unwrap(),
            Some(TokenUrl::new(token_url.clone()).unwrap()),
        )
        .set_redirect_uri(
            RedirectUrl::new(format!(
                "{}/auth/oauth/{}/callback",
                config.oauth_redirect_base_url,
                self.as_str()
            ))
            .unwrap(),
        )
    }
}
```

Add `pub mod oauth;` to `src/auth/mod.rs`.

- [ ] **Step 4: Add `find_or_create_oauth_user` to `src/models/user.rs`**

```rust
pub async fn find_or_create_oauth_user(
    pool: &PgPool,
    provider: &str,
    provider_user_id: &str,
    email: &str,
) -> Result<User, sqlx::Error> {
    if let Some((user_id,)) = sqlx::query_as::<_, (Uuid,)>(
        "select user_id from oauth_identities where provider = $1 and provider_user_id = $2",
    )
    .bind(provider)
    .bind(provider_user_id)
    .fetch_optional(pool)
    .await?
    {
        return sqlx::query_as::<_, User>("select * from users where id = $1")
            .bind(user_id)
            .fetch_one(pool)
            .await;
    }

    let mut tx = pool.begin().await?;
    let user: User = sqlx::query_as(
        "insert into users (email, email_verified_at) values ($1, now()) returning *",
    )
    .bind(email)
    .fetch_one(&mut *tx)
    .await?;

    sqlx::query(
        "insert into oauth_identities (user_id, provider, provider_user_id) values ($1, $2, $3)",
    )
    .bind(user.id)
    .bind(provider)
    .bind(provider_user_id)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(user)
}
```

- [ ] **Step 5: Add OAuth start/callback routes to `src/routes/auth.rs`**

```rust
use axum::extract::Path;
use oauth2::{CsrfToken, Scope, AuthorizationCode, TokenResponse, reqwest::async_http_client};

// add to router():
.route("/auth/oauth/:provider/start", get(oauth_start))
.route("/auth/oauth/:provider/callback", get(oauth_callback))
```

```rust
async fn oauth_start(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    session: Session,
) -> Result<axum::response::Redirect, AppError> {
    let provider = auth::oauth::Provider::from_str(&provider)
        .ok_or_else(|| AppError::Validation("unknown provider".into()))?;
    let client = provider.client(&state.config);

    let (auth_url, csrf_token) = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new("email".to_string()))
        .url();

    session
        .insert("oauth_csrf", csrf_token.secret().clone())
        .await
        .map_err(|_| AppError::Validation("session error".into()))?;

    Ok(axum::response::Redirect::to(auth_url.as_str()))
}

#[derive(Deserialize)]
struct OAuthCallbackQuery {
    code: String,
    state: String,
}

#[derive(serde::Deserialize)]
struct OAuthUserInfo {
    id: String,
    email: String,
}

async fn oauth_callback(
    State(state): State<AppState>,
    Path(provider): Path<String>,
    Query(query): Query<OAuthCallbackQuery>,
    session: Session,
) -> Result<Json<Value>, AppError> {
    let provider = auth::oauth::Provider::from_str(&provider)
        .ok_or_else(|| AppError::Validation("unknown provider".into()))?;

    let expected_csrf: String = session
        .get("oauth_csrf")
        .await
        .map_err(|_| AppError::Validation("session error".into()))?
        .ok_or_else(|| AppError::Validation("missing csrf state".into()))?;
    if expected_csrf != query.state {
        return Err(AppError::Validation("csrf mismatch".into()));
    }

    let client = provider.client(&state.config);
    let token = client
        .exchange_code(AuthorizationCode::new(query.code))
        .request_async(async_http_client)
        .await
        .map_err(|_| AppError::Validation("token exchange failed".into()))?;

    let userinfo: OAuthUserInfo = reqwest::Client::new()
        .get(provider.userinfo_url(&state.config))
        .bearer_auth(token.access_token().secret())
        .send()
        .await
        .map_err(|_| AppError::Validation("userinfo request failed".into()))?
        .json()
        .await
        .map_err(|_| AppError::Validation("invalid userinfo response".into()))?;

    let user = models::user::find_or_create_oauth_user(
        &state.db,
        provider.as_str(),
        &userinfo.id,
        &userinfo.email,
    )
    .await?;

    session.remove::<String>("oauth_csrf").await.ok();
    session
        .insert("user_id", user.id)
        .await
        .map_err(|_| AppError::Validation("session error".into()))?;
    session
        .insert("auth_at", chrono::Utc::now())
        .await
        .map_err(|_| AppError::Validation("session error".into()))?;

    Ok(Json(json!({ "authenticated": true })))
}
```

- [ ] **Step 6: Add the `url` crate for parsing the redirect in the test**

```bash
cargo add url --dev
```

- [ ] **Step 7: Write the failing test `tests/oauth.rs`**

```rust
use axum::{body::Body, http::{Request, StatusCode, header}};
use tower::ServiceExt;
use nami_engine::{state::AppState, email::Mailer, routes, config::Config};
use wiremock::{MockServer, Mock, ResponseTemplate};
use wiremock::matchers::{method, path};

fn test_config(mock_server_uri: &str) -> Config {
    let mut config = Config::from_env_for_test();
    config.google_auth_url = format!("{mock_server_uri}/authorize");
    config.google_token_url = format!("{mock_server_uri}/token");
    config.google_userinfo_url = format!("{mock_server_uri}/userinfo");
    config
}

#[sqlx::test]
async fn oauth_callback_creates_and_logs_in_user(pool: sqlx::PgPool) {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "fake-token", "token_type": "bearer"
        })))
        .mount(&mock_server)
        .await;
    Mock::given(method("GET")).and(path("/userinfo"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "google-123", "email": "oauth@example.com"
        })))
        .mount(&mock_server)
        .await;

    let config = test_config(&mock_server.uri());
    let state = AppState { db: pool.clone(), mailer: Mailer::new("localhost", 1025), config };
    let app = axum::Router::new()
        .merge(routes::auth::router())
        .layer(tower_sessions::SessionManagerLayer::new(tower_sessions::MemoryStore::default()))
        .with_state(state);

    let start_res = app
        .clone()
        .oneshot(Request::builder().uri("/auth/oauth/google/start").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let session_cookie = start_res.headers().get(header::SET_COOKIE).unwrap().clone();
    let location = start_res.headers().get(header::LOCATION).unwrap().to_str().unwrap().to_string();
    let real_state = url::Url::parse(&location)
        .unwrap()
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .to_string();

    let callback_res = app
        .oneshot(
            Request::builder()
                .uri(format!("/auth/oauth/google/callback?code=fake-code&state={real_state}"))
                .header(header::COOKIE, session_cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(callback_res.status(), StatusCode::OK);

    let count: (i64,) = sqlx::query_as("select count(*) from users where email = $1")
        .bind("oauth@example.com")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count.0, 1);
}
```

- [ ] **Step 8: Run test to verify it fails, then passes**

Run: `cargo test --test oauth`
Expected: FAIL before routes exist, PASS after.

- [ ] **Step 9: Commit**

```bash
git add nami-engine/src/auth/oauth.rs nami-engine/src/auth/mod.rs nami-engine/src/config.rs \
  nami-engine/src/models/user.rs nami-engine/src/routes/auth.rs nami-engine/src/state.rs \
  nami-engine/src/main.rs nami-engine/tests/oauth.rs nami-engine/Cargo.toml nami-engine/Cargo.lock
git commit -m "feat(engine): add Google/GitHub OAuth login"
```

---

### Task 11: Global RBAC and minimal project CRUD

**Files:**
- Create: `nami-engine/src/middleware/mod.rs`
- Create: `nami-engine/src/middleware/rbac.rs`
- Create: `nami-engine/src/models/project.rs`
- Create: `nami-engine/src/routes/projects.rs`
- Modify: `nami-engine/src/routes/mod.rs`
- Modify: `nami-engine/src/main.rs`
- Test: `nami-engine/tests/rbac.rs`

**Interfaces:**
- Produces: `CurrentUser { id: Uuid, email: String, global_role: String }` (an Axum `FromRequestParts` extractor backed by the session), `middleware::rbac::require_global_role(user: &CurrentUser, min: &str) -> Result<(), AppError>`, `models::project::Project`, `models::project::insert_project`, `models::project::list_projects_for_user`, routes `POST /projects`, `GET /projects`.

- [ ] **Step 1: Write the `CurrentUser` extractor in `src/middleware/rbac.rs`**

```rust
use axum::{extract::FromRequestParts, http::request::Parts};
use tower_sessions::Session;
use crate::{state::AppState, error::AppError, models};

pub struct CurrentUser {
    pub id: uuid::Uuid,
    pub email: String,
    pub global_role: String,
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let session = Session::from_request_parts(parts, state)
            .await
            .map_err(|_| AppError::Unauthorized)?;

        let user_id: uuid::Uuid = session
            .get("user_id")
            .await
            .map_err(|_| AppError::Unauthorized)?
            .ok_or(AppError::Unauthorized)?;

        let user: models::user::User = sqlx::query_as("select * from users where id = $1")
            .bind(user_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::Unauthorized)?;

        Ok(CurrentUser { id: user.id, email: user.email, global_role: user.global_role })
    }
}

pub fn require_global_role(user: &CurrentUser, min: &str) -> Result<(), AppError> {
    let rank = |role: &str| match role {
        "admin" => 2,
        "member" => 1,
        "viewer" => 0,
        _ => -1,
    };
    if rank(&user.global_role) >= rank(min) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}
```

Write `src/middleware/mod.rs`:

```rust
pub mod rbac;
```

Add `pub mod middleware;` to `src/lib.rs`.

- [ ] **Step 2: Write `src/models/project.rs`**

```rust
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, sqlx::FromRow, serde::Serialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
}

pub async fn insert_project(pool: &PgPool, name: &str, slug: &str) -> Result<Project, sqlx::Error> {
    sqlx::query_as("insert into projects (name, slug) values ($1, $2) returning id, name, slug")
        .bind(name)
        .bind(slug)
        .fetch_one(pool)
        .await
}

pub async fn list_projects_for_user(pool: &PgPool, user_id: Uuid, is_global_admin: bool) -> Result<Vec<Project>, sqlx::Error> {
    if is_global_admin {
        sqlx::query_as("select id, name, slug from projects").fetch_all(pool).await
    } else {
        sqlx::query_as(
            "select p.id, p.name, p.slug from projects p
             join project_members pm on pm.project_id = p.id
             where pm.user_id = $1",
        )
        .bind(user_id)
        .fetch_all(pool)
        .await
    }
}
```

- [ ] **Step 3: Write `src/routes/projects.rs`**

```rust
use axum::{routing::post, Router, Json, extract::State};
use serde::Deserialize;
use crate::{state::AppState, error::AppError, models, middleware::rbac::{CurrentUser, require_global_role}};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects", post(create_project).get(list_projects))
}

#[derive(Deserialize)]
struct CreateProjectRequest {
    name: String,
    slug: String,
}

async fn create_project(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(payload): Json<CreateProjectRequest>,
) -> Result<Json<models::project::Project>, AppError> {
    require_global_role(&user, "admin")?;
    let project = models::project::insert_project(&state.db, &payload.name, &payload.slug).await?;
    Ok(Json(project))
}

async fn list_projects(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<Vec<models::project::Project>>, AppError> {
    let projects = models::project::list_projects_for_user(&state.db, user.id, user.global_role == "admin").await?;
    Ok(Json(projects))
}
```

Add `pub mod projects;` to `src/routes/mod.rs` and `.merge(routes::projects::router())` in `main.rs`.

- [ ] **Step 4: Write the failing test `tests/rbac.rs`**

```rust
use axum::{body::Body, http::{Request, StatusCode, header}};
use tower::ServiceExt;
use nami_engine::{state::AppState, email::Mailer, routes};
use serde_json::json;

fn test_app(pool: sqlx::PgPool) -> axum::Router {
    let state = AppState {
        db: pool,
        mailer: Mailer::new("localhost", 1025),
        config: nami_engine::config::Config::from_env_for_test(),
    };
    axum::Router::new()
        .merge(routes::auth::router())
        .merge(routes::projects::router())
        .layer(tower_sessions::SessionManagerLayer::new(tower_sessions::MemoryStore::default()))
        .with_state(state)
}

async fn login_as(app: axum::Router, email: &str) -> axum::http::HeaderValue {
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(json!({"email": email, "password": "password123"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    res.headers().get(header::SET_COOKIE).unwrap().clone()
}

#[sqlx::test]
async fn viewer_cannot_create_project(pool: sqlx::PgPool) {
    let hash = nami_engine::auth::password::hash_password("password123").unwrap();
    sqlx::query("insert into users (email, password_hash, email_verified_at, global_role) values ($1, $2, now(), 'viewer')")
        .bind("viewer@example.com")
        .bind(&hash)
        .execute(&pool)
        .await
        .unwrap();

    let app = test_app(pool.clone());
    let cookie = login_as(app.clone(), "viewer@example.com").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/projects")
                .header("content-type", "application/json")
                .header(header::COOKIE, cookie)
                .body(Body::from(json!({"name": "Test", "slug": "test"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn admin_can_create_project(pool: sqlx::PgPool) {
    let hash = nami_engine::auth::password::hash_password("password123").unwrap();
    sqlx::query("insert into users (email, password_hash, email_verified_at, global_role) values ($1, $2, now(), 'admin')")
        .bind("admin@example.com")
        .bind(&hash)
        .execute(&pool)
        .await
        .unwrap();

    let app = test_app(pool.clone());
    let cookie = login_as(app.clone(), "admin@example.com").await;

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/projects")
                .header("content-type", "application/json")
                .header(header::COOKIE, cookie)
                .body(Body::from(json!({"name": "Test", "slug": "test"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
```

- [ ] **Step 5: Run tests to verify they fail, then pass**

Run: `cargo test --test rbac`
Expected: FAIL before RBAC/projects routes exist, PASS after.

- [ ] **Step 6: Commit**

```bash
git add nami-engine/src/middleware nami-engine/src/models/project.rs \
  nami-engine/src/routes/projects.rs nami-engine/src/routes/mod.rs nami-engine/src/main.rs \
  nami-engine/src/lib.rs nami-engine/tests/rbac.rs
git commit -m "feat(engine): add global RBAC guard and minimal project CRUD"
```

---

### Task 12: Project-scoped RBAC and membership management

**Files:**
- Modify: `nami-engine/src/middleware/rbac.rs`
- Modify: `nami-engine/src/models/project.rs`
- Modify: `nami-engine/src/routes/projects.rs`
- Modify: `nami-engine/tests/rbac.rs`

**Interfaces:**
- Consumes: `CurrentUser` from Task 11.
- Produces: `middleware::rbac::require_project_role(pool, user: &CurrentUser, project_id: Uuid, min: &str) -> Result<(), AppError>`, `models::project::upsert_member`, `models::project::remove_member`; routes `GET /projects/:id`, `POST /projects/:id/members`, `DELETE /projects/:id/members/:user_id`.

- [ ] **Step 1: Add `require_project_role` to `src/middleware/rbac.rs`**

```rust
pub async fn require_project_role(
    pool: &sqlx::PgPool,
    user: &CurrentUser,
    project_id: uuid::Uuid,
    min: &str,
) -> Result<(), crate::error::AppError> {
    if user.global_role == "admin" {
        return Ok(());
    }

    let row: Option<(String,)> = sqlx::query_as(
        "select project_role from project_members where project_id = $1 and user_id = $2",
    )
    .bind(project_id)
    .bind(user.id)
    .fetch_optional(pool)
    .await?;

    let rank = |role: &str| match role {
        "admin" => 2,
        "member" => 1,
        "viewer" => 0,
        _ => -1,
    };

    match row {
        Some((role,)) if rank(&role) >= rank(min) => Ok(()),
        _ => Err(crate::error::AppError::Forbidden),
    }
}
```

- [ ] **Step 2: Add membership helpers to `src/models/project.rs`**

```rust
pub async fn upsert_member(pool: &PgPool, project_id: Uuid, user_id: Uuid, role: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "insert into project_members (project_id, user_id, project_role) values ($1, $2, $3)
         on conflict (project_id, user_id) do update set project_role = excluded.project_role",
    )
    .bind(project_id)
    .bind(user_id)
    .bind(role)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn remove_member(pool: &PgPool, project_id: Uuid, user_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("delete from project_members where project_id = $1 and user_id = $2")
        .bind(project_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<Project>, sqlx::Error> {
    sqlx::query_as("select id, name, slug from projects where id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await
}
```

- [ ] **Step 3: Add scoped routes to `src/routes/projects.rs`**

```rust
use axum::{extract::Path, routing::get};
use crate::middleware::rbac::require_project_role;

// add to router():
.route("/projects/:id", get(get_project))
.route("/projects/:id/members", post(add_member))
.route("/projects/:id/members/:user_id", axum::routing::delete(delete_member))
```

```rust
async fn get_project(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<uuid::Uuid>,
) -> Result<Json<models::project::Project>, AppError> {
    require_project_role(&state.db, &user, id, "viewer").await?;
    let project = models::project::find_by_id(&state.db, id).await?.ok_or(AppError::NotFound)?;
    Ok(Json(project))
}

#[derive(Deserialize)]
struct AddMemberRequest {
    user_id: uuid::Uuid,
    project_role: String,
}

async fn add_member(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<uuid::Uuid>,
    Json(payload): Json<AddMemberRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_project_role(&state.db, &user, id, "admin").await?;
    models::project::upsert_member(&state.db, id, payload.user_id, &payload.project_role).await?;
    Ok(Json(serde_json::json!({ "added": true })))
}

async fn delete_member(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, member_id)): Path<(uuid::Uuid, uuid::Uuid)>,
) -> Result<Json<serde_json::Value>, AppError> {
    require_project_role(&state.db, &user, id, "admin").await?;
    models::project::remove_member(&state.db, id, member_id).await?;
    Ok(Json(serde_json::json!({ "removed": true })))
}
```

- [ ] **Step 4: Append the failing test to `tests/rbac.rs`**

Covers Review Focus item: a user with no `project_members` row and not global admin gets 403 from a project-scoped route.

```rust
#[sqlx::test]
async fn member_without_project_row_gets_forbidden(pool: sqlx::PgPool) {
    let hash = nami_engine::auth::password::hash_password("password123").unwrap();
    sqlx::query("insert into users (email, password_hash, email_verified_at, global_role) values ($1, $2, now(), 'member')")
        .bind("member@example.com")
        .bind(&hash)
        .execute(&pool)
        .await
        .unwrap();
    let project_id: (uuid::Uuid,) = sqlx::query_as(
        "insert into projects (name, slug) values ('Test', 'test') returning id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    let app = test_app(pool.clone());
    let cookie = login_as(app.clone(), "member@example.com").await;

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/projects/{}", project_id.0))
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[sqlx::test]
async fn global_admin_bypasses_project_membership(pool: sqlx::PgPool) {
    let hash = nami_engine::auth::password::hash_password("password123").unwrap();
    sqlx::query("insert into users (email, password_hash, email_verified_at, global_role) values ($1, $2, now(), 'admin')")
        .bind("admin2@example.com")
        .bind(&hash)
        .execute(&pool)
        .await
        .unwrap();
    let project_id: (uuid::Uuid,) = sqlx::query_as(
        "insert into projects (name, slug) values ('Test2', 'test2') returning id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    let app = test_app(pool.clone());
    let cookie = login_as(app.clone(), "admin2@example.com").await;

    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/projects/{}", project_id.0))
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
```

- [ ] **Step 5: Run tests to verify they fail, then pass**

Run: `cargo test --test rbac`
Expected: FAIL before project-scoped routes exist, PASS after.

- [ ] **Step 6: Commit**

```bash
git add nami-engine/src/middleware/rbac.rs nami-engine/src/models/project.rs \
  nami-engine/src/routes/projects.rs nami-engine/tests/rbac.rs
git commit -m "feat(engine): add project-scoped RBAC and membership management"
```

---

### Task 13: Rate limiting on register and login

**Files:**
- Modify: `nami-engine/src/routes/auth.rs`
- Modify: `nami-engine/src/main.rs`
- Test: `nami-engine/tests/rate_limit.rs`

**Interfaces:**
- Produces: `routes::auth::rate_limited_router() -> Router<AppState>` (register + login only), `routes::auth::router() -> Router<AppState>` (everything else, unchanged name, reduced scope); a `tower_governor::GovernorLayer` applied only to the former.

- [ ] **Step 1: Add dependency**

```bash
cd nami-engine && cargo add tower_governor
```

- [ ] **Step 2: Split the auth router in `src/routes/auth.rs`**

Change `pub fn router()` to only mount the non-rate-limited routes, and add:

```rust
pub fn rate_limited_router() -> Router<AppState> {
    Router::new()
        .route("/auth/register", post(register))
        .route("/auth/login", post(login))
}
```

Remove the `/auth/register` and `/auth/login` lines from the existing `router()` function body (they move to `rate_limited_router()`).

- [ ] **Step 3: Apply the layer in `main.rs`**

```rust
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};
use std::sync::Arc;

let governor_conf = Arc::new(
    GovernorConfigBuilder::default()
        .per_second(60)
        .burst_size(5)
        .finish()
        .unwrap(),
);

let app = Router::new()
    .merge(routes::health::router())
    .merge(routes::auth::router())
    .merge(routes::auth::rate_limited_router().layer(GovernorLayer { config: governor_conf }))
    .merge(routes::projects::router())
    .layer(session_layer)
    .with_state(state);
```

- [ ] **Step 4: Write the failing test `tests/rate_limit.rs`**

```rust
use axum::{body::Body, http::{Request, StatusCode}};
use tower::ServiceExt;
use nami_engine::{state::AppState, email::Mailer, routes, config::Config};
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};
use std::sync::Arc;
use serde_json::json;

#[sqlx::test]
async fn sixth_rapid_login_attempt_is_rate_limited(pool: sqlx::PgPool) {
    let state = AppState { db: pool, mailer: Mailer::new("localhost", 1025), config: Config::from_env_for_test() };
    let governor_conf = Arc::new(
        GovernorConfigBuilder::default().per_second(60).burst_size(5).finish().unwrap(),
    );
    let app = routes::auth::rate_limited_router()
        .layer(GovernorLayer { config: governor_conf })
        .with_state(state);

    let mut last_status = StatusCode::OK;
    for _ in 0..6 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/auth/login")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"email": "nobody@example.com", "password": "x"}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        last_status = response.status();
    }

    assert_eq!(last_status, StatusCode::TOO_MANY_REQUESTS);
}
```

- [ ] **Step 5: Run test to verify it fails, then passes**

Run: `cargo test --test rate_limit`
Expected: FAIL before the split router + layer exist, PASS after.

- [ ] **Step 6: Commit**

```bash
git add nami-engine/src/routes/auth.rs nami-engine/src/main.rs nami-engine/tests/rate_limit.rs \
  nami-engine/Cargo.toml nami-engine/Cargo.lock
git commit -m "feat(engine): rate-limit register and login"
```

---

### Task 14: nami-ui BFF wiring

**Files:**
- Create: `nami-ui/.env.example`
- Create: `nami-ui/src/lib/server/engine.ts`
- Create: `nami-ui/src/lib/server/engine.test.ts`
- Create: `nami-ui/src/routes/register/+page.server.ts`
- Create: `nami-ui/src/routes/register/+page.svelte`
- Create: `nami-ui/src/routes/login/+page.server.ts`
- Create: `nami-ui/src/routes/login/+page.svelte`
- Modify: `nami-ui/package.json`

**Interfaces:**
- Produces: `callEngine(event: RequestEvent, path: string, init?: RequestInit) -> Promise<Response>` — forwards the incoming `cookie` header to `nami-engine` and relays any `set-cookie` response header back onto `event.cookies`.

- [ ] **Step 1: Add vitest**

```bash
cd nami-ui && npm install -D vitest
```

Add to `package.json` scripts: `"test": "vitest run"`.

- [ ] **Step 2: Write `.env.example`**

```
ENGINE_URL=http://localhost:8080
```

- [ ] **Step 3: Write the failing test `src/lib/server/engine.test.ts`**

```typescript
import { describe, it, expect, vi } from 'vitest';
import { callEngine } from './engine';

describe('callEngine', () => {
	it('forwards the cookie header and relays set-cookie back', async () => {
		const fetchMock = vi.fn().mockResolvedValue(
			new Response(JSON.stringify({ ok: true }), {
				status: 200,
				headers: { 'set-cookie': 'id=abc123; Path=/; HttpOnly' }
			})
		);
		const setCookie = vi.fn();

		const event = {
			fetch: fetchMock,
			request: { headers: new Headers({ cookie: 'id=existing' }) },
			cookies: { set: setCookie }
		} as any;

		const response = await callEngine(event, '/auth/login', { method: 'POST' });

		expect(fetchMock).toHaveBeenCalledWith(
			'http://localhost:8080/auth/login',
			expect.objectContaining({
				method: 'POST',
				headers: expect.objectContaining({ cookie: 'id=existing' })
			})
		);
		expect(setCookie).toHaveBeenCalledWith('id', 'abc123', expect.objectContaining({ path: '/' }));
		expect(response.status).toBe(200);
	});
});
```

- [ ] **Step 4: Run test to verify it fails**

Run: `npm test`
Expected: FAIL (`engine.ts` doesn't exist yet)

- [ ] **Step 5: Write `src/lib/server/engine.ts`**

```typescript
import type { RequestEvent } from '@sveltejs/kit';
import { env } from '$env/dynamic/private';

export async function callEngine(
	event: RequestEvent,
	path: string,
	init: RequestInit = {}
): Promise<Response> {
	const engineUrl = env.ENGINE_URL ?? 'http://localhost:8080';
	const headers = new Headers(init.headers);
	const incomingCookie = event.request.headers.get('cookie');
	if (incomingCookie) headers.set('cookie', incomingCookie);
	if (!headers.has('content-type') && init.body) headers.set('content-type', 'application/json');

	const response = await (event.fetch ?? fetch)(`${engineUrl}${path}`, { ...init, headers });

	const setCookie = response.headers.get('set-cookie');
	if (setCookie) {
		const [pair, ...attrs] = setCookie.split(';').map((s) => s.trim());
		const [name, value] = pair.split('=');
		const pathAttr = attrs.find((a) => a.toLowerCase().startsWith('path='));
		event.cookies.set(name, value, {
			path: pathAttr ? pathAttr.split('=')[1] : '/',
			httpOnly: attrs.some((a) => a.toLowerCase() === 'httponly')
		});
	}

	return response;
}
```

- [ ] **Step 6: Run test to verify it passes**

Run: `npm test`
Expected: PASS

- [ ] **Step 7: Write `src/routes/register/+page.server.ts`**

```typescript
import type { Actions } from './$types';
import { callEngine } from '$lib/server/engine';

export const actions: Actions = {
	default: async (event) => {
		const form = await event.request.formData();
		const response = await callEngine(event, '/auth/register', {
			method: 'POST',
			body: JSON.stringify({
				email: form.get('email'),
				password: form.get('password')
			})
		});
		const body = await response.json();
		return { success: response.ok, message: body.message ?? body.error };
	}
};
```

- [ ] **Step 8: Write `src/routes/register/+page.svelte`**

```svelte
<script lang="ts">
	import type { ActionData } from './$types';
	let { form }: { form: ActionData } = $props();
</script>

<form method="POST">
	<label>Email <input name="email" type="email" required /></label>
	<label>Password <input name="password" type="password" required minlength="8" /></label>
	<button type="submit">Register</button>
</form>
{#if form?.message}
	<p>{form.message}</p>
{/if}
```

- [ ] **Step 9: Write `src/routes/login/+page.server.ts`**

```typescript
import type { Actions } from './$types';
import { redirect } from '@sveltejs/kit';
import { callEngine } from '$lib/server/engine';

export const actions: Actions = {
	default: async (event) => {
		const form = await event.request.formData();
		const response = await callEngine(event, '/auth/login', {
			method: 'POST',
			body: JSON.stringify({
				email: form.get('email'),
				password: form.get('password')
			})
		});
		const body = await response.json();
		if (!response.ok) {
			return { success: false, message: body.error ?? 'login failed' };
		}
		if (body.requires_totp) {
			return { success: true, requiresTotp: true };
		}
		throw redirect(303, '/');
	}
};
```

- [ ] **Step 10: Write `src/routes/login/+page.svelte`**

```svelte
<script lang="ts">
	import type { ActionData } from './$types';
	let { form }: { form: ActionData } = $props();
</script>

<form method="POST">
	<label>Email <input name="email" type="email" required /></label>
	<label>Password <input name="password" type="password" required /></label>
	<button type="submit">Log in</button>
</form>
{#if form?.message}
	<p>{form.message}</p>
{/if}
{#if form?.requiresTotp}
	<p>Enter your 2FA code to continue.</p>
{/if}
```

- [ ] **Step 11: Manual end-to-end check**

Run: `docker compose -f nami-engine/docker-compose.yaml up -d`, `cd nami-engine && cargo run`, `cd nami-ui && ENGINE_URL=http://localhost:8080 npm run dev` — open `/register` in a browser, register, open Mailpit at `http://localhost:8025`, confirm the verification email arrived, then log in at `/login` and confirm a `Set-Cookie` round-trips in devtools' network tab.

- [ ] **Step 12: Commit**

```bash
git add nami-ui/.env.example nami-ui/src/lib/server nami-ui/src/routes/register \
  nami-ui/src/routes/login nami-ui/package.json nami-ui/package-lock.json
git commit -m "feat(ui): add BFF proxy and register/login pages"
```
