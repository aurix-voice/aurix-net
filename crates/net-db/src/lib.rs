//! PostgreSQL access: pool, embedded migrations, row types and queries.

pub mod models;
pub mod queries;

use net_common::config::DatabaseConfig;
use net_common::{NetError, Result};
use sqlx::postgres::PgPoolOptions;

pub type DbPool = sqlx::PgPool;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("../../migrations");

pub async fn create_pool(cfg: &DatabaseConfig) -> Result<DbPool> {
    let pool = PgPoolOptions::new()
        .max_connections(cfg.max_connections)
        .connect(&cfg.url)
        .await
        .map_err(|e| NetError::Database(format!("connect: {e}")))?;
    if cfg.run_migrations {
        MIGRATOR
            .run(&pool)
            .await
            .map_err(|e| NetError::Database(format!("migrate: {e}")))?;
    }
    Ok(pool)
}

pub(crate) fn db_err(e: sqlx::Error) -> NetError {
    match e {
        sqlx::Error::RowNotFound => NetError::NotFound("row".into()),
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            NetError::Conflict(d.message().to_string())
        }
        other => NetError::Database(other.to_string()),
    }
}
