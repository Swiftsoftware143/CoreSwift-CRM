//! CRM Swift — Multi-account Lead Management Operating System
//!
//! Each "account" is backed by a DB tenant. "Team members" are users under an account.
//! This is the main entry point for the Axum-based REST API server.
//! The server provides a fully-featured CRM with contacts, pipelines,
//! lead scoring, automation, and integration capabilities.

mod config;
mod db;
mod errors;
pub mod auth;
pub mod account;
pub mod contacts;
pub mod contacts_internal;
pub mod companies;
pub mod pipelines;
pub mod tags;
pub mod scoring;
pub mod lists;
pub mod lists_internal;
pub mod automation;
pub mod integrations;
pub mod analytics;
pub mod ai;
pub mod billing;
pub mod affiliates;
pub mod audit;
pub mod events;
pub mod communications;
pub mod checklists;
pub mod monitoring;
pub mod notifications;
pub mod native_apps;
pub mod admin_actions;
pub mod campaigns;
pub mod industries;
pub mod plans;
pub mod provider_keys;
pub mod rate_limiter;
pub mod webhook;
pub mod dashboard;
pub mod portfolio;
pub mod inbound;
pub mod telnyx;
pub mod webhooks;
pub mod worker;

use axum::{
    routing::get,
    Router,
    response::IntoResponse,
    http::{StatusCode, HeaderValue},
};
use tokio::signal;
use tower_http::{
    cors::CorsLayer,
    trace::TraceLayer,
    compression::CompressionLayer,
    services::ServeDir,
};
use tracing_subscriber::EnvFilter;
use std::time::Duration;

/// Application state shared across all handlers
#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub redis: redis::aio::ConnectionManager,
    pub config: config::AppConfig,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("config", &self.config)
            .finish()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing with structured logging
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(true)
        .with_thread_ids(true)
        .init();

    // Load environment variables
    let config = config::AppConfig::from_env()?;

    // Connect to PostgreSQL with connection pooling
    let db = db::connect(
        &config.database_url,
        config.db_min_connections,
        config.db_max_connections,
    ).await?;

    // Run database migrations (skip if already applied)
    match sqlx::migrate!("./migrations")
        .run(&db)
        .await
    {
        Ok(_) => tracing::info!("Database migrations completed successfully"),
        Err(e) => tracing::warn!("Migration skipped (tables may already exist): {}", e),
    }

    // Connect to Redis
    let redis = db::connect_redis(&config.redis_url).await?;
    tracing::info!("Connected to Redis");

    // Build shared state
    let state = AppState {
        db,
        redis,
        config: config.clone(),
    };

    // Request ID middleware — adds X-Request-Id to every response
    let request_id_middleware = axum::middleware::from_fn(request_id_middleware_fn);

    // Build the complete router
    let app = Router::new()
        // Health check (no auth required)
        .route("/api/health", get(health_check))
        .route("/api/ready", get(ready_check))
        // Serve SPA at root
        .nest_service("/", ServeDir::new("public"))
        // Auth routes (no auth required)
        .nest("/api/auth", auth::router())
        // Protected routes
        .nest("/api/account", account::router(state.clone()))
        .nest("/api/contacts", contacts::router(state.clone()))
        .nest("/api/internal/contacts", contacts_internal::router())
        .nest("/api/companies", companies::router(state.clone()))
        .nest("/api/pipelines", pipelines::router(state.clone()))
        .nest("/api/tags", tags::router(state.clone()))
        .nest("/api/scoring", scoring::router(state.clone()))
        .nest("/api/lists", lists::router(state.clone()))
        .nest("/api/internal/lists", lists_internal::router())
        .nest("/api/internal/tags", tags::internal_handler::router())
        .nest("/api/analytics", analytics::router(state.clone()))
        .nest("/api/ai", ai::router(state.clone()))
        // Billing (plan tiers, feature toggles)
        .nest("/api/campaigns", campaigns::router(state.clone()))
        .nest("/api/billing", billing::router(state.clone()))
        // Affiliates (referral tracking, commissions)
        .nest("/api/affiliates", affiliates::router(state.clone()))
        // Audit logs (system-wide event trail)
        .nest("/api/audit", audit::router(state.clone()))
        // Event webhook hub
        .nest("/api/events", events::router(state.clone()))
        // Communications (Twilio/SendGrid orchestration)
        .nest("/api/comms", communications::router(state.clone()))
        // Native app connectors (AdaSwift, FunnelSwift, CheatLayer, etc.)
        .nest("/api/native", native_apps::router(state.clone()))
        // Public webhook — single endpoint for OpenClaw, n8n, CheatLayer
        .nest("/api/webhook", webhook::router())
        // Dashboard — aggregate stats
        .nest("/api/dashboard", dashboard::router(state.clone()))
        // Portfolio — multi-entity portfolio companies
        .nest("/api/portfolio", portfolio::router(state.clone()))
        // Inbound webhook — receive events from satellite apps
        .nest("/inbound", inbound::router())
        // Admin chat actions — run the entire business from Telegram
        .nest("/api/admin", admin_actions::router(state.clone()))
        // Onboarding checklists
        .nest("/api/checklists", checklists::router(state.clone()))
        // Account health monitoring
        .nest("/api/monitoring", monitoring::router(state.clone()))
        // In-app notifications
        .nest("/api/notifications", notifications::router(state.clone()))
        // Telnyx SMS/Voice integration
        .nest("/api/telnyx", telnyx::router(state.clone()))
        // Cross-app webhooks — receive tag sync events from satellite apps
        .nest("/api/v1/webhooks", webhooks::cross_app_tag_sync::router())
        // Layer stack (inner to outer = last to first in call order)
        .layer(CompressionLayer::new())
        .layer(axum::middleware::from_fn(security_headers_middleware))
        .layer(request_id_middleware)
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state.clone());

    // Start background worker for delayed actions, inactive trials, health recalculation
    let db_for_worker = state.db.clone();
    tokio::spawn(async move {
        if let Err(e) = worker::start_worker(db_for_worker).await {
            tracing::error!(error = %e, "Failed to start background worker");
        }
    });

    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!("Starting CRM Swift server on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;

    // Graceful shutdown with CTRL+C
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

/// Health check endpoint — returns 200 when the service is running
async fn health_check() -> impl IntoResponse {
    (StatusCode::OK, axum::Json(serde_json::json!({
        "status": "ok",
        "service": "crm-swift",
        "version": env!("CARGO_PKG_VERSION")
    })))
}

/// Readiness check endpoint — verifies database connectivity
async fn ready_check(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> impl IntoResponse {
    // Quick DB ping to verify connectivity
    let db_ok = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.db)
        .await
        .is_ok();

    use redis::AsyncCommands;
    let mut redis_conn = state.redis.clone();
    let redis_ok: bool = redis_conn.set::<&str, &str, String>("healthcheck", "ok").await.is_ok();

    if db_ok && redis_ok {
        (StatusCode::OK, axum::Json(serde_json::json!({
            "status": "ready",
            "database": "connected",
            "redis": "connected",
        })))
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, axum::Json(serde_json::json!({
            "status": "not_ready",
            "database": db_ok,
            "redis": redis_ok,
        })))
    }
}

/// Request ID middleware — generates and attaches a UUID to each request
async fn request_id_middleware_fn(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    let request_id = uuid::Uuid::new_v4().to_string();
    req.extensions_mut().insert(RequestId(request_id.clone()));

    tracing::Span::current().record("request_id", request_id.as_str());

    let mut response = next.run(req).await;
    response.headers_mut().insert(
        "X-Request-Id",
        HeaderValue::from_str(&request_id).unwrap_or_else(|_| HeaderValue::from_static("unknown")),
    );
    response
}

/// Request ID wrapper for extension storage
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

/// Security headers middleware (Helmet-like)
async fn security_headers_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    let mut response = next.run(req).await;

    // Security headers
    response.headers_mut().insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        "X-Frame-Options",
        HeaderValue::from_static("DENY"),
    );
    response.headers_mut().insert(
        "X-XSS-Protection",
        HeaderValue::from_static("1; mode=block"),
    );
    response.headers_mut().insert(
        "Referrer-Policy",
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    response.headers_mut().insert(
        "Permissions-Policy",
        HeaderValue::from_static("geolocation=(), microphone=(), camera=()"),
    );

    response
}

/// Graceful shutdown handler — listens for SIGTERM/SIGINT
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("Ctrl+C received, starting graceful shutdown");
        }
        _ = terminate => {
            tracing::info!("SIGTERM received, starting graceful shutdown");
        }
    }

    // Give in-flight requests time to complete
    tokio::time::sleep(Duration::from_millis(500)).await;
    tracing::info!("Server shutdown complete");
}
