//! Provisioning console (kanban t_e968e9ad) — the operator's half of the tag → free account
//! contract.
//!
//! `GET /api/admin/provisioning-config` — the two knobs plus this app's own free plans.
//! `PUT /api/admin/provisioning-config` — save them.
//!
//! There is no second writer of these keys: the admin console is the only editor, and the account
//! door (`POST /api/v1/internal/provision-free-account`) only ever READS them. Both live in
//! [`crate::tag_provision_handler`], so the shape the console writes is exactly the shape the door
//! reads.
//!
//! Platform-operator only: the route sits on `admin_actions::router`'s protected branch, which
//! carries `require_platform_admin_middleware` on the ROUTER (so a route added here cannot ship
//! ungated) — see the note there. A tenant `owner` gets 403.

use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::errors::AppError;
use crate::tag_provision_handler::{
    free_plans, read_provisioning_settings, save_provisioning_settings, ProvisioningSettings,
    PROVISION_ENABLED_KEY, PROVISION_ENTRY_PLAN_KEY,
};
use crate::AppState;

/// The writable half of the panel. Both fields are optional so the toggle and the plan picker can
/// be saved independently (`None` leaves that knob untouched).
#[derive(Debug, Deserialize)]
pub struct UpdateProvisioningSettings {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub entry_plan_slug: Option<String>,
}

pub async fn get_config(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let settings = read_provisioning_settings(&state.db).await?;
    Ok(Json(payload(&state, &settings).await?))
}

pub async fn update_config(
    State(state): State<AppState>,
    Json(req): Json<UpdateProvisioningSettings>,
) -> Result<Json<Value>, AppError> {
    // The entry plan must resolve IN THIS APP to a free, active plan — the same predicate the mint
    // seats with (`auth::signup::entry_plan_id`), so the picker cannot save a slug the door would
    // then refuse to mint on.
    if let Some(slug) = req.entry_plan_slug.as_deref() {
        let slug = slug.trim();
        if slug.is_empty() {
            return Err(AppError::Validation(
                "entry_plan_slug must not be empty".into(),
            ));
        }
        if crate::auth::signup::entry_plan_id(&state.db, slug)
            .await?
            .is_none()
        {
            return Err(AppError::Validation(format!(
                "'{slug}' is not one of this app's free, active plans"
            )));
        }
    }

    save_provisioning_settings(&state.db, req.enabled, req.entry_plan_slug.as_deref()).await?;

    // Answer from the STORE, never from the request: what the panel shows next is what the door
    // will actually read.
    let settings = read_provisioning_settings(&state.db).await?;
    Ok(Json(payload(&state, &settings).await?))
}

async fn payload(state: &AppState, settings: &ProvisioningSettings) -> Result<Value, AppError> {
    let plans = free_plans(&state.db).await?;
    let entry_plan_resolves =
        crate::auth::signup::entry_plan_id(&state.db, &settings.entry_plan_slug)
            .await?
            .is_some();
    Ok(json!({
        "enabled": settings.enabled,
        "entry_plan_slug": settings.entry_plan_slug,
        "entry_plan_resolves": entry_plan_resolves,
        "free_plans": plans
            .iter()
            .map(|(slug, name)| json!({ "slug": slug, "name": name }))
            .collect::<Vec<_>>(),
        "setting_keys": {
            "enabled": PROVISION_ENABLED_KEY,
            "entry_plan_slug": PROVISION_ENTRY_PLAN_KEY,
        },
        "endpoint": "/api/v1/internal/provision-free-account",
        "source_app": "funnelswift",
    }))
}
