//! ACCOUNT MATCHING — the 7th back-end specialist module.
//!
//! Keeps the pipeline clean by matching an inbound identity to the contacts and companies a tenant
//! already has, instead of letting every capture path invent its own answer (or no answer) and
//! letting the same person in twice.
//!
//! # What this module owns
//!
//! * `engine::resolve` — the cascade (normalized email → normalized phone → name + company) against
//!   the caller's own tenant, with the arm that answered and a confidence.
//! * `engine::duplicate_groups` — the duplicate report (normalized phone / normalized name) for one
//!   tenant. Email cannot be a key: `idx_contacts_tenant_email` is UNIQUE inside a tenant.
//! * `engine::find_contact_by_email_exact` — the exact-email lookup the contact-create path uses,
//!   moved here so the module is the ONE implementation of identity lookup rather than a third copy
//!   of it (`contacts::handlers::create` now calls this).
//!
//! # Its own plan tier
//!
//! Registered like every other module: a `modules` row (`key = 'account_match'`) plus a
//! `plan_modules` assignment row per plan, seeded by migration 113 — granted from `starter` up and
//! NOT part of `free`, the same shape as Support tickets and the Private mailbox. The admin's
//! Features & Plans matrix reads the registry, so this module appears there automatically and its
//! tier is changed with one press, no deploy. The module's own router carries
//! `crate::features::gate_mw`, so the plan flag is ENFORCED here, not merely declared: a tenant whose
//! plan does not grant account matching is answered **402** with the module's label.
//!
//! The engine's library functions carry no gate of their own — the contact-create path they serve is
//! gated by `limit_max_contacts` and must not begin refusing writes because a plan changed.
//!
//! # Tenancy
//!
//! The tenant always comes from the caller's JWT (`Claims::aid`) or from the credential the calling
//! path already resolved, and is bound into every statement. No statement in this module can read
//! another tenant's rows.

pub mod engine;
pub mod handlers;

use crate::AppState;
use axum::{middleware, Router};

pub fn router(state: AppState) -> Router<AppState> {
    Router::new()
        .route("/resolve", axum::routing::post(handlers::resolve))
        .route("/duplicates", axum::routing::get(handlers::duplicates))
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        // Plan gating — the admin controls this module per plan (the module & feature registry is
        // the source of truth for the admin UI; see src/module_registry).
        .layer(middleware::from_fn_with_state(
            crate::features::FeatureGate::new(
                state.db.clone(),
                "account_match",
                "Account matching",
            ),
            crate::features::gate_mw,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
