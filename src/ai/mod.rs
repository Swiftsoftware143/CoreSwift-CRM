//! AI Intelligence Layer — the brain of the Flawless Follow-up system.
//!
//! This module transforms rigid "If-Not-Then" rules into adaptive,
//! context-aware decisions. Instead of firing the same email template
//! at every inactive user after 24 hours, the AI decides:
//!
//! - WHICH channel to use (email vs SMS vs hybrid)
//! - WHAT message to send (personalized to their behavior)
//! - WHEN to send (optimal delivery window)
//! - WHETHER to escalate to a human
//!
//! The AI reads from account_health signals, event_logs, checklist_progress,
//! and contact scoring to make these decisions at runtime.

pub mod engine;
pub mod handlers;
pub mod models;
pub mod router;

use crate::AppState;
use axum::{middleware, Router};

/// Build the AI router with auth middleware.
pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/prioritize", axum::routing::post(handlers::prioritize))
        .route("/predict", axum::routing::post(handlers::predict))
        .route("/recommend", axum::routing::post(handlers::recommend))
        .route("/campaign", axum::routing::post(handlers::campaign))
        .route("/message", axum::routing::post(handlers::compose_message))
        .route("/channel", axum::routing::post(handlers::suggest_channel))
        .route("/timing", axum::routing::post(handlers::suggest_timing))
        .route("/risk", axum::routing::post(handlers::assess_churn_risk))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
