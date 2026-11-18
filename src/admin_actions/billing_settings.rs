//! Platform billing settings (admin): how a customer pays while card payments are off.
//!
//! Routes:
//!   GET /api/admin/billing-settings — the operator's saved words (empty string = feature off)
//!   PUT /api/admin/billing-settings — save them (`{"payment_instructions": "..."}`)
//!
//! Mounted on `admin_actions::router`'s PROTECTED half, which wraps every route with
//! `auth::platform_admin::require_platform_admin_middleware` — a tenant `role='owner'` reaches
//! neither arm. The gate sits on the ROUTER, not in these handlers, so the two arms cannot drift
//! into different answers.
//!
//! Why this exists: CoreSwift has no card till at all (`checkout_sessions` was dropped by
//! migration 097), so a paid plan is bought by ASKING — the customer presses "Request this plan"
//! and a person switches the workspace on. The customer was never told how to send the money. The
//! operator writes it here; `billing_instructions::load` publishes it on the anonymous price list
//! (`GET /api/v1/public/plans`) that the marketing pricing page and the customer plan ladder read.
//!
//! Fleet parity: FunnelSwift, IncentiveSwift, ADASwift, WorkflowSwift and MissedCall Respondr.

use crate::billing_instructions;
use crate::errors::{ApiResult, AppError};
use crate::AppState;
use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Value};

/// GET /api/admin/billing-settings
pub async fn get_billing_settings(State(state): State<AppState>) -> ApiResult<Json<Value>> {
    let saved = billing_instructions::load(&state.db).await;
    Ok(Json(json!({
        "payment_instructions": saved.clone().unwrap_or_default(),
        "payment_instructions_set": saved.is_some(),
        "max_length": billing_instructions::MAX_LEN,
        "message": if saved.is_some() {
            "A visitor on the pricing page and a signed-in buyer both see these words."
        } else {
            "Nothing is shown to a buyer until this is filled in."
        },
    })))
}

#[derive(Deserialize)]
pub struct BillingSettingsBody {
    /// The operator's own words. Absent keeps the stored value; an empty string CLEARS it (the
    /// feature goes back to off).
    pub payment_instructions: Option<String>,
}

/// PUT /api/admin/billing-settings
pub async fn update_billing_settings(
    State(state): State<AppState>,
    Json(body): Json<BillingSettingsBody>,
) -> ApiResult<Json<Value>> {
    let Some(text) = body.payment_instructions else {
        return Err(AppError::Validation(
            "Send {\"payment_instructions\": \"...\"}. An empty string clears it.".to_string(),
        ));
    };
    billing_instructions::save(&state.db, &text)
        .await
        .map_err(AppError::Validation)?;

    let saved = billing_instructions::load(&state.db).await;
    Ok(Json(json!({
        "success": true,
        "payment_instructions": saved.clone().unwrap_or_default(),
        "payment_instructions_set": saved.is_some(),
    })))
}
