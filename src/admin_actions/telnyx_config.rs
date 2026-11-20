//! Telnyx inbound signing key — the operator's half (kanban t_e2017eb0).
//!
//! `GET  /api/admin/telnyx-config` — whether inbound verification is READY, which store carries the
//!                                   key, and a mask of the stored value.
//! `PUT  /api/admin/telnyx-config` — paste the account's Ed25519 public key, or clear it.
//!
//! WHY. Both public Telnyx receivers (`POST /api/telnyx/webhook`, `POST /api/telnyx/sms-webhook`)
//! fail CLOSED while no account public key is configured: every delivery is answered
//! `503 telnyx_verification_not_configured`, so inbound calls and inbound SMS — the entire
//! integration — cannot work. That key existed ONLY as `TELNYX_PUBLIC_KEY` in the deploy
//! environment, i.e. a root-owned file plus a container restart, while every other Telnyx value is
//! set from the admin console's Telnyx panel. The receivers now read ENV FIRST, then
//! `telnyx_config.public_key` (see `telnyx::handlers::resolve_signing_key`), so this route is the
//! second, restart-free way in — and the one the panel below writes.
//!
//! Platform-operator only: the route sits on `admin_actions::router`'s protected branch, which
//! carries `require_platform_admin_middleware` on the ROUTER (so a route added here cannot ship
//! ungated). A tenant `owner` gets 403.

use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::errors::AppError;
use crate::telnyx::handlers::{resolve_signing_key, validate_public_key};
use crate::AppState;

/// The writable half of the panel. `public_key` pastes a key; `clear_public_key` empties the stored
/// one; neither = leave it alone (so saving the panel for another reason cannot wipe the key, and a
/// console that round-trips the mask it was shown cannot overwrite the real value with the mask).
#[derive(Debug, Deserialize)]
pub struct UpdateSigningKey {
    #[serde(default)]
    pub public_key: Option<String>,
    #[serde(default)]
    pub clear_public_key: Option<bool>,
}

/// A short, non-reversible-looking preview of the stored key, for the panel. A public key is not a
/// secret, but the panel only needs to say WHICH key is in force.
fn preview(key: &str) -> Option<String> {
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 12 {
        return Some(format!("{}…", chars.iter().collect::<String>()));
    }
    let head: String = chars.iter().take(6).collect();
    let tail: String = chars.iter().skip(chars.len() - 4).collect();
    Some(format!("{head}…{tail}"))
}

/// The stored value on the row the receivers read (the active global config row).
async fn stored_key(state: &AppState) -> Result<Option<String>, AppError> {
    let stored: Option<String> =
        sqlx::query_scalar("SELECT public_key FROM telnyx_config WHERE is_active = true LIMIT 1")
            .fetch_optional(&state.db)
            .await?;
    Ok(stored
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty()))
}

async fn payload(state: &AppState) -> Result<Value, AppError> {
    let stored = stored_key(state).await?;
    let (effective, source) =
        resolve_signing_key(state.config.telnyx_public_key.as_deref(), &state.db).await?;
    let env_set = state
        .config
        .telnyx_public_key
        .as_deref()
        .map(str::trim)
        .map(|k| !k.is_empty())
        .unwrap_or(false);
    let note = if effective.is_some() {
        if env_set {
            "Inbound verification is ON, using the key in the server environment (TELNYX_PUBLIC_KEY). A key saved here is ignored while the environment carries one."
        } else {
            "Inbound verification is ON, using the key saved from this panel. Real Telnyx deliveries can now be applied."
        }
    } else {
        "Inbound verification is OFF: both Telnyx receivers answer 503 telnyx_verification_not_configured and apply nothing. Paste the account's Ed25519 public key from Telnyx Mission Control to turn it on."
    };
    Ok(json!({
        "configured": effective.is_some(),
        "source": source.as_str(),
        "env_set": env_set,
        "stored_public_key_set": stored.is_some(),
        "public_key_preview": preview(stored.as_deref().unwrap_or("")),
        "receivers": ["POST /api/telnyx/webhook", "POST /api/telnyx/sms-webhook"],
        "note": note,
    }))
}

pub async fn get_config(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    Ok(Json(payload(&state).await?))
}

pub async fn update_config(
    State(state): State<AppState>,
    Json(req): Json<UpdateSigningKey>,
) -> Result<Json<Value>, AppError> {
    let existing_id: Option<uuid::Uuid> =
        sqlx::query_scalar("SELECT id FROM telnyx_config WHERE is_active = true LIMIT 1")
            .fetch_optional(&state.db)
            .await?;
    let stored = stored_key(&state).await?.unwrap_or_default();

    let submitted = req
        .public_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let new_value = if req.clear_public_key.unwrap_or(false) {
        String::new()
    } else if let Some(supplied) = submitted {
        // A console that posts back the mask it was shown means "keep it".
        if Some(supplied.clone()) == preview(&stored) {
            stored.clone()
        } else {
            validate_public_key(&supplied).map_err(AppError::Validation)?
        }
    } else {
        stored.clone()
    };

    match existing_id {
        Some(id) => {
            sqlx::query(
                "UPDATE telnyx_config SET public_key = $1, updated_at = NOW() WHERE id = $2",
            )
            .bind(&new_value)
            .bind(id)
            .execute(&state.db)
            .await?;
        }
        None => {
            sqlx::query(
                "INSERT INTO telnyx_config (id, public_key, is_active, created_at, updated_at)
                 VALUES ($1, $2, true, NOW(), NOW())",
            )
            .bind(uuid::Uuid::new_v4())
            .bind(&new_value)
            .execute(&state.db)
            .await?;
        }
    }

    tracing::info!(
        "Telnyx inbound signing key {} from the admin console",
        if new_value.is_empty() {
            "CLEARED"
        } else {
            "saved"
        }
    );

    Ok(Json(payload(&state).await?))
}
