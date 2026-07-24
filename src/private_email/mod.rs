use axum::{Router, middleware};
use crate::AppState;

pub mod domain_handler;
pub mod mailbox_handler;
pub mod send_handler;
pub mod webhook_handler;
pub mod models;
pub mod encryption;
pub mod feature_gate;

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        // Domain management
        .route("/domains", axum::routing::get(domain_handler::list_domains))
        .route("/domains", axum::routing::post(domain_handler::add_domain))
        .route("/domains/:id", axum::routing::patch(domain_handler::update_domain))
        .route("/domains/:id", axum::routing::delete(domain_handler::delete_domain))
        // Mailbox management
        .route("/boxes", axum::routing::get(mailbox_handler::list_mailboxes))
        .route("/boxes", axum::routing::post(mailbox_handler::provision_mailbox))
        .route("/boxes/:id", axum::routing::patch(mailbox_handler::update_mailbox))
        .route("/boxes/:id", axum::routing::delete(mailbox_handler::delete_mailbox))
        // Send email
        .route("/send", axum::routing::post(send_handler::send_email))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}
