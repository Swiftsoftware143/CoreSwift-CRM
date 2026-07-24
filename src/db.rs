//! Database connection and pool setup.
//!
//! Provides PostgreSQL connection pooling and Redis connection management.

use sqlx::postgres::PgPoolOptions;
use redis::aio::ConnectionManager;

/// Connect to PostgreSQL and return a connection pool.
/// Uses configurable min/max connections. Defaults to 5/20.
pub async fn connect(
    database_url: &str,
    min_connections: u32,
    max_connections: u32,
) -> Result<sqlx::PgPool, anyhow::Error> {
    let pool = PgPoolOptions::new()
        .min_connections(min_connections)
        .max_connections(max_connections)
        .connect(database_url)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to PostgreSQL: {}", e))?;

    tracing::info!(
        "Connected to PostgreSQL (pool: {}/{})",
        min_connections, max_connections
    );
    Ok(pool)
}

/// Connect to Redis and return a connection manager.
pub async fn connect_redis(redis_url: &str) -> Result<ConnectionManager, anyhow::Error> {
    let client = redis::Client::open(redis_url)
        .map_err(|e| anyhow::anyhow!("Failed to create Redis client: {}", e))?;

    let conn = ConnectionManager::new(client)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to connect to Redis: {}", e))?;

    tracing::info!("Connected to Redis");
    Ok(conn)
}
