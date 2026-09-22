//! Plans module — the one live endpoint left in it.
//!
//! This module used to carry a full plan CRUD (`list`/`create`/`get`/`update`/`delete`) behind a
//! `claims.role == "agency_admin"` check. None of it was reachable: `plans::router()` was never
//! nested in `main.rs`, and **every real user carries `role = 'owner'`**, so the role test could
//! never pass for anybody. The live plan endpoints are `/api/billing/plans*` (billing::handlers),
//! and the admin console manages gating through `/api/admin/*` (module registry).
//!
//! The dead handlers went with this change. They were not harmless: they were the only code that
//! still declared the phantom `plans` columns `max_deals`, `max_users`, `max_storage_mb` and
//! `payment_link`, so any future lane that wired one of those routes up would have shipped a 500.
//! The FunnelSwift affiliate-product sync helper went with them (it was reachable only from the
//! dead create/update/delete) — it is in git history if plan CRUD is ever built for real.
//!
//! What is left is mounted at `GET /api/billing/plans/registry` and gated on the REAL
//! platform-admin claim (`users.is_platform_admin`), the same gate as every other admin surface.

use axum::{
    extract::{Extension, State},
    response::IntoResponse,
    Json,
};
use serde_json::json;

use crate::auth::models::Claims;
use crate::errors::ApiResult;
use crate::AppState;

/// The boolean feature keys the admin can toggle per plan, plus every plan's current values.
/// Read straight from the module registry tables — the catalogue is DATA, so a newly registered
/// module appears here with no code change (it used to be a Rust const). Shape kept identical for
/// the existing admin console:
/// `{ "features": [{key, label, module, note, is_active}], "plans": [{slug, name, features}] }`.
pub async fn feature_registry(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    // Was `claims.role != "agency_admin"` -> 403 for literally everyone, including the platform
    // admin. Authority is now the real flag, resolved server-side.
    crate::auth::platform_admin::require_platform_admin(&s.db, &c.sub).await?;

    // Explicit columns rather than `SELECT *`: the old `Plan` model declared columns this table
    // does not have, so a star-select failed to map and the endpoint 500ed.
    let rows: Vec<(String, String, serde_json::Value)> = sqlx::query_as(
        "SELECT slug, name, COALESCE(features, '{}'::jsonb) AS features
           FROM plans
          ORDER BY sort_order ASC NULLS LAST, name ASC",
    )
    .fetch_all(&s.db)
    .await?;

    let feats: Vec<(String, String, String, Option<String>, bool)> = sqlx::query_as(
        "SELECT f.key, f.name, m.key, f.description, f.is_active
           FROM module_features f JOIN modules m ON m.id = f.module_id
          WHERE f.kind = 'boolean' AND m.is_active
          ORDER BY m.sort_order, f.sort_order, f.key",
    )
    .fetch_all(&s.db)
    .await?;

    Ok(Json(json!({
        "features": feats
            .into_iter()
            .map(|(key, label, module, note, is_active)| json!({
                "key": key,
                "label": label,
                "module": module,
                "note": note.unwrap_or_default(),
                "is_active": is_active,
            }))
            .collect::<Vec<_>>(),
        "plans": rows
            .into_iter()
            .map(|(slug, name, features)| json!({ "slug": slug, "name": name, "features": features }))
            .collect::<Vec<_>>(),
    })))
}
