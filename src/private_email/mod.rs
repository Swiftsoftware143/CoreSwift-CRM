use crate::AppState;
use axum::{middleware, Router};

pub mod admin_handler;
pub mod api_keys_handler;
pub mod auto_reply_handler;
pub mod domain_handler;
pub mod encryption;
pub mod feature_gate;
pub mod mailbox_handler;
pub mod messages_handler;
pub mod models;
pub mod providers;
pub mod purge;
pub mod send_handler;
pub mod storage;
pub mod trial;
pub mod webhook_handler;

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        // Domain management
        .route("/domains", axum::routing::get(domain_handler::list_domains))
        .route("/domains", axum::routing::post(domain_handler::add_domain))
        .route(
            "/domains/:id",
            axum::routing::patch(domain_handler::update_domain),
        )
        .route(
            "/domains/:id",
            axum::routing::delete(domain_handler::delete_domain),
        )
        // Re-read the provider's own state for this domain (t_f02ade57): what makes a domain that
        // Mailgun activates AFTER it was added become usable without re-entering the credential.
        .route(
            "/domains/:id/verify",
            axum::routing::post(domain_handler::verify_domain),
        )
        // The records the customer must add, in plain English (B205 setup page). Values come from
        // Mailgun itself, never from a doc.
        .route(
            "/domains/:id/records",
            axum::routing::get(domain_handler::domain_records),
        )
        // Mailbox management
        .route(
            "/boxes",
            axum::routing::get(mailbox_handler::list_mailboxes),
        )
        .route(
            "/boxes",
            axum::routing::post(mailbox_handler::provision_mailbox),
        )
        .route(
            "/boxes/:id",
            axum::routing::patch(mailbox_handler::update_mailbox),
        )
        .route(
            "/boxes/:id",
            axum::routing::delete(mailbox_handler::delete_mailbox),
        )
        // ── The FREE mailbox's trial state (CEO spec / David 2026-11-11, BYOK) ───────────────────
        // `GET /mailbox-status` is the console's countdown + fair-use meter, computed from the same
        // registry rows the send path enforces. There is deliberately NO CoreSwift-hosted trial
        // address: a free account brings its own domain and Mailgun account, verifies the domain,
        // and only then does a mailbox (and the 30-day clock) exist.
        .route(
            "/mailbox-status",
            axum::routing::get(trial::mailbox_status_handler),
        )
        // ── Messages: the inbox's own records (B204/B205/B211) ──────────────────────────────────
        // Every received message, the address it arrived on, and whether it also became a ticket.
        // The inbox reads THESE, not tickets — which is what makes mail to an unticketed address
        // (info@, david@) visible instead of acknowledged-and-invisible.
        .route(
            "/messages",
            axum::routing::get(messages_handler::list_messages),
        )
        // Static segment before the capture, so /messages/counts is never read as an id.
        .route(
            "/messages/counts",
            axum::routing::get(messages_handler::message_counts),
        )
        // The composer's own store — Drafts has to be able to hold something (B205).
        .route(
            "/messages/drafts",
            axum::routing::post(messages_handler::save_draft),
        )
        .route(
            "/messages/:id",
            axum::routing::get(messages_handler::get_message)
                .patch(messages_handler::update_message)
                .delete(messages_handler::delete_message),
        )
        .route(
            "/messages/:id/ticket",
            axum::routing::post(messages_handler::create_ticket_from_message),
        )
        .route(
            "/messages/:id/reply",
            axum::routing::post(messages_handler::reply_to_message),
        )
        // One-click summary of a thread, using the account's own AI provider. Refuses with a plain
        // sentence when no key is configured, so the client can hide the control.
        .route(
            "/messages/:id/summarize",
            axum::routing::post(messages_handler::summarize_message),
        )
        // The people a message can be assigned to — this account's members only.
        .route(
            "/members",
            axum::routing::get(messages_handler::list_members),
        )
        // The address filter's source of truth: every address this account receives on, plus MISC.
        .route(
            "/addresses",
            axum::routing::get(messages_handler::list_addresses),
        )
        // The rail: system folders with live counts plus real subfolders.
        .route(
            "/folders",
            axum::routing::get(messages_handler::list_folders)
                .post(messages_handler::create_folder),
        )
        // Mark all read / unread, and empty Spam or Trash (destructive, always reports a count).
        .route(
            "/folders/action",
            axum::routing::post(messages_handler::folder_action),
        )
        // Labels: name + colour, per account, applied to one or many messages.
        .route(
            "/labels",
            axum::routing::get(messages_handler::list_labels).post(messages_handler::create_label),
        )
        .route(
            "/labels/:id",
            axum::routing::patch(messages_handler::update_label)
                .delete(messages_handler::delete_label),
        )
        .route(
            "/labels/:id/apply",
            axum::routing::post(messages_handler::apply_label)
                .delete(messages_handler::remove_label),
        )
        // Send email
        .route("/send", axum::routing::post(send_handler::send_email))
        // API Keys (named, reusable)
        .route("/keys", axum::routing::get(api_keys_handler::list_api_keys))
        .route("/keys", axum::routing::post(api_keys_handler::add_api_key))
        .route(
            "/keys/:id",
            axum::routing::delete(api_keys_handler::delete_api_key),
        )
        // Auto-reply / sequences
        .route(
            "/auto-replies",
            axum::routing::get(auto_reply_handler::list_auto_replies),
        )
        .route(
            "/auto-replies",
            axum::routing::post(auto_reply_handler::create_auto_reply),
        )
        .route(
            "/auto-replies/:id",
            axum::routing::patch(auto_reply_handler::update_auto_reply),
        )
        .route(
            "/auto-replies/:id",
            axum::routing::delete(auto_reply_handler::delete_auto_reply),
        )
        // Admin controls (agency_admin only)
        .route(
            "/admin/limits",
            axum::routing::get(admin_handler::list_all_overrides),
        )
        .route(
            "/admin/limits/:tenant_id",
            axum::routing::get(admin_handler::get_tenant_limits),
        )
        .route(
            "/admin/limits/:tenant_id",
            axum::routing::patch(admin_handler::set_tenant_limits),
        )
        // Retention settings
        .route(
            "/admin/limits/:tenant_id/retention",
            axum::routing::get(admin_handler::get_retention),
        )
        .route(
            "/admin/limits/:tenant_id/retention",
            axum::routing::patch(admin_handler::set_retention),
        )
        // Manual purge trigger
        .route(
            "/admin/purge/run",
            axum::routing::post(admin_handler::trigger_purge),
        )
        .route(
            "/support-box",
            axum::routing::get(admin_handler::get_support_box).put(admin_handler::set_support_box),
        )
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
