use axum::{Router, middleware};
use crate::AppState;

pub mod domain_handler;
pub mod mailbox_handler;
pub mod send_handler;
pub mod webhook_handler;
pub mod models;
pub mod encryption;
pub mod feature_gate;
pub mod api_keys_handler;
pub mod auto_reply_handler;

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
        // API Keys (named, reusable)
        .route("/keys", axum::routing::get(api_keys_handler::list_api_keys))
        .route("/keys", axum::routing::post(api_keys_handler::add_api_key))
        .route("/keys/:id", axum::routing::delete(api_keys_handler::delete_api_key))
        // Auto-reply / sequences
        .route("/auto-replies", axum::routing::get(auto_reply_handler::list_auto_replies))
        .route("/auto-replies", axum::routing::post(auto_reply_handler::create_auto_reply))
        .route("/auto-replies/:id", axum::routing::patch(auto_reply_handler::update_auto_reply))
        .route("/auto-replies/:id", axum::routing::delete(auto_reply_handler::delete_auto_reply))
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::middleware::auth_middleware))
}
