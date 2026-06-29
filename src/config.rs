//! Application configuration loaded from environment variables.
//!
//! All configuration is read from environment variables or a `.env` file.
//! Sensible defaults are provided for development.

use std::env;

/// Application configuration
#[derive(Clone, Debug)]
pub struct AppConfig {
    pub host: String,
    pub port: u16,
    pub database_url: String,
    pub redis_url: String,
    pub jwt_secret: String,
    pub jwt_access_expiry: i64,
    pub jwt_refresh_expiry: i64,
    pub auth_rate_limit_per_minute: u32,
    pub api_rate_limit_per_minute: u32,
    pub score_cache_ttl: u64,
    pub list_cache_ttl: u64,
    pub session_cache_ttl: u64,
    pub db_min_connections: u32,
    pub db_max_connections: u32,
    pub internal_sync_key: String,
}

impl AppConfig {
    /// Load configuration from environment variables with sensible defaults.
    ///
    /// # Panics
    ///
    /// Panics if `DATABASE_URL` or `JWT_SECRET` are not set.
    pub fn from_env() -> Result<Self, anyhow::Error> {
        // Load .env file if it exists
        dotenvy::dotenv().ok();

        let host = env::var("APP_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
        let port = env::var("APP_PORT")
            .unwrap_or_else(|_| "8080".to_string())
            .parse::<u16>()
            .map_err(|e| anyhow::anyhow!("Invalid APP_PORT: {}", e))?;

        let database_url = env::var("DATABASE_URL")
            .map_err(|_| anyhow::anyhow!("DATABASE_URL environment variable is required"))?;

        let redis_url = env::var("REDIS_URL")
            .unwrap_or_else(|_| "redis://localhost:6379".to_string());

        let jwt_secret = env::var("JWT_SECRET")
            .map_err(|_| anyhow::anyhow!("JWT_SECRET environment variable is required"))?;

        #[allow(clippy::unwrap_used)]
        let jwt_access_expiry = env::var("JWT_ACCESS_TOKEN_EXPIRY")
            .unwrap_or_else(|_| "3600".to_string())
            .parse::<i64>()
            .map_err(|e| anyhow::anyhow!("Invalid JWT_ACCESS_TOKEN_EXPIRY: {}", e))?;

        #[allow(clippy::unwrap_used)]
        let jwt_refresh_expiry = env::var("JWT_REFRESH_TOKEN_EXPIRY")
            .unwrap_or_else(|_| "2592000".to_string())
            .parse::<i64>()
            .map_err(|e| anyhow::anyhow!("Invalid JWT_REFRESH_TOKEN_EXPIRY: {}", e))?;

        #[allow(clippy::unwrap_used)]
        let auth_rate_limit_per_minute = env::var("AUTH_RATE_LIMIT_PER_MINUTE")
            .unwrap_or_else(|_| "5".to_string())
            .parse::<u32>()
            .map_err(|e| anyhow::anyhow!("Invalid AUTH_RATE_LIMIT_PER_MINUTE: {}", e))?;

        #[allow(clippy::unwrap_used)]
        let api_rate_limit_per_minute = env::var("API_RATE_LIMIT_PER_MINUTE")
            .unwrap_or_else(|_| "20".to_string())
            .parse::<u32>()
            .map_err(|e| anyhow::anyhow!("Invalid API_RATE_LIMIT_PER_MINUTE: {}", e))?;

        #[allow(clippy::unwrap_used)]
        let score_cache_ttl = env::var("SCORE_CACHE_TTL")
            .unwrap_or_else(|_| "300".to_string())
            .parse::<u64>()
            .map_err(|e| anyhow::anyhow!("Invalid SCORE_CACHE_TTL: {}", e))?;

        #[allow(clippy::unwrap_used)]
        let list_cache_ttl = env::var("LIST_CACHE_TTL")
            .unwrap_or_else(|_| "120".to_string())
            .parse::<u64>()
            .map_err(|e| anyhow::anyhow!("Invalid LIST_CACHE_TTL: {}", e))?;

        #[allow(clippy::unwrap_used)]
        let session_cache_ttl = env::var("SESSION_CACHE_TTL")
            .unwrap_or_else(|_| "3600".to_string())
            .parse::<u64>()
            .map_err(|e| anyhow::anyhow!("Invalid SESSION_CACHE_TTL: {}", e))?;

        #[allow(clippy::unwrap_used)]
        let db_min_connections = env::var("DB_MIN_CONNECTIONS")
            .unwrap_or_else(|_| "5".to_string())
            .parse::<u32>()
            .map_err(|e| anyhow::anyhow!("Invalid DB_MIN_CONNECTIONS: {}", e))?;

        #[allow(clippy::unwrap_used)]
        let db_max_connections = env::var("DB_MAX_CONNECTIONS")
            .unwrap_or_else(|_| "20".to_string())
            .parse::<u32>()
            .map_err(|e| anyhow::anyhow!("Invalid DB_MAX_CONNECTIONS: {}", e))?;

        let internal_sync_key = env::var("INTERNAL_SYNC_KEY")
            .unwrap_or_else(|_| "".to_string());

        Ok(AppConfig {
            host,
            port,
            database_url,
            redis_url,
            jwt_secret,
            jwt_access_expiry,
            jwt_refresh_expiry,
            auth_rate_limit_per_minute,
            api_rate_limit_per_minute,
            score_cache_ttl,
            list_cache_ttl,
            session_cache_ttl,
            db_min_connections,
            db_max_connections,
            internal_sync_key,
        })
    }
}
