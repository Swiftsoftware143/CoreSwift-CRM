//! Admin: the PLATFORM mail transport — the provider and credential every workspace WITHOUT its
//! own sending identity falls back to (kanban t_6a330ed2).
//!
//! GET    /api/admin/email-config        masked view: is it configured, and WHICH store has it
//! PUT    /api/admin/email-config        save provider / api_url / api_key / from_address / from_name
//! DELETE /api/admin/email-config        remove the stored override (back to the server environment)
//! POST   /api/admin/email-config/test   send a real message through the platform transport
//!
//! PLATFORM-ADMIN ONLY. These routes live on `admin_actions::router`'s protected half, which is
//! gated by `auth::platform_admin::require_platform_admin_middleware` — a tenant `role='owner'`
//! reaches none of them.
//!
//! The credential is written to the SAME place the delivery path reads
//! (`admin_settings` key `email`, via `communications::platform_mail_config`), so a rotation takes
//! effect on the next delivery with no redeploy and no container recreate. Nothing on any read path
//! returns the key: the GET serves a mask, a "set" flag and the key's LENGTH, and never a digest of
//! it (a fingerprint would be a brute-force oracle for a short secret).

use axum::{extract::State, Extension, Json};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::communications::platform_mail_config as platform;
use crate::communications::providers;
use crate::errors::AppError;
use crate::AppState;

/// The row as it is STORED (not opened) — used only to tell "a row exists" from "no row".
async fn stored_raw(db: &PgPool) -> Option<Value> {
    sqlx::query_scalar::<_, Value>("SELECT value FROM admin_settings WHERE key = $1")
        .bind(platform::CONFIG_KEY)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
}

/// The whole answer both GET and the write routes return, so the panel always re-renders from the
/// state the DATABASE actually holds rather than from the payload it just sent.
async fn view(state: &AppState) -> Value {
    let raw = stored_raw(&state.db).await;
    // Masked from the OPENED row: `api_key_len` must describe the credential that is stored, not
    // the length of its envelope. `stored` is the module's own reader, so this is the same value
    // the delivery path would use; a row this deployment cannot open reports "not set" rather than
    // a length nobody can honour.
    let opened = platform::stored(&state.db)
        .await
        .unwrap_or_else(|| json!({}));
    let masked = platform::masked_config(&opened);
    let effective = platform::resolved(&state.db).await;

    json!({
        "config": masked,
        "stored": raw.is_some(),
        "configured": effective.is_some(),
        "provider": masked.get("provider").cloned().unwrap_or(Value::Null),
        "providers": platform::providers_json(),
        "effective": effective.as_ref().map(|t| t.view()),
        "env": platform::env_view(),
        "updated_at": opened.get("updated_at").cloned().unwrap_or(Value::Null),
        "updated_by": opened.get("updated_by").cloned().unwrap_or(Value::Null),
    })
}

/// GET /api/admin/email-config — masked view + which store is carrying platform mail.
pub async fn get_config(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    Ok(Json(view(&state).await))
}

/// The calling admin's own address, for the test-send default destination.
async fn admin_email(db: &PgPool, sub: &str) -> Option<String> {
    let id = Uuid::parse_str(sub).ok()?;
    let email = sqlx::query_scalar::<_, Option<String>>("SELECT email FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(db)
        .await
        .inspect_err(|e| tracing::warn!(error = %e, "admin email lookup failed"))
        .ok()
        .flatten()
        .flatten()
        .map(|e| e.trim().to_string())
        .filter(|e| !e.is_empty());
    email
}

/// A trimmed string field of the request body, or `None` when absent/null.
fn body_str(body: &Value, key: &str) -> Option<String> {
    body.get(key)
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_string())
}

/// PUT /api/admin/email-config — save the platform transport.
///
/// A masked or absent `api_key` keeps the stored credential (the panel pre-fills the mask); an
/// explicitly EMPTY one clears it. The saved value is sealed before it is written, so a database
/// dump never yields a usable platform key.
pub async fn update_config(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, AppError> {
    if !body.is_object() {
        return Err(AppError::BadRequest("Expected a JSON object".to_string()));
    }
    // The stored row, opened: what a masked round-trip must preserve.
    let existing = platform::stored(&state.db)
        .await
        .unwrap_or_else(|| json!({}));

    let provider = body_str(&body, "provider")
        .filter(|p| !p.is_empty())
        .or_else(|| platform::field(&existing, "provider"))
        .unwrap_or_else(|| platform::DEFAULT_PROVIDER.to_string());
    if !platform::is_supported_provider(&provider) {
        return Err(AppError::BadRequest(format!(
            "Unknown email provider '{}'. This app's platform transport carries: {}. A workspace's \
             own SMTP/SendGrid credentials are configured in that workspace's Integration Center, \
             not here.",
            provider,
            platform::provider_values().join(", ")
        )));
    }

    let api_url = match body_str(&body, "api_url") {
        Some(url) => url,
        None => platform::field(&existing, "api_url").unwrap_or_default(),
    };
    if !api_url.is_empty() && !(api_url.starts_with("https://") || api_url.starts_with("http://")) {
        return Err(AppError::BadRequest(format!(
            "api_url must be the full send endpoint (e.g. https://api.mailgun.net/v3/<domain>/messages) — got '{api_url}'."
        )));
    }

    let from_address = match body_str(&body, "from_address") {
        Some(address) => address,
        None => platform::field(&existing, "from_address").unwrap_or_default(),
    };
    if !from_address.is_empty() && !from_address.contains('@') {
        return Err(AppError::BadRequest(format!(
            "'{from_address}' is not an email address — From must be an address on the domain the endpoint sends through."
        )));
    }

    let from_name = match body_str(&body, "from_name") {
        Some(name) => name,
        None => platform::field(&existing, "from_name").unwrap_or_default(),
    };

    // The credential precedence, in one place: absent or masked = keep what is stored (re-sealing
    // the same plaintext below), empty = clear, anything else = the new credential.
    let stored_key = platform::field(&existing, "api_key").unwrap_or_default();
    let api_key = match body.get("api_key") {
        None | Some(Value::Null) => stored_key,
        Some(Value::String(s)) => {
            let s = s.trim();
            if s.is_empty() {
                String::new()
            } else if s == platform::MASK || s.contains("...") {
                stored_key
            } else {
                if s.chars().any(char::is_whitespace) {
                    return Err(AppError::BadRequest(
                        "The API key must not contain spaces or line breaks — paste the key on its own."
                            .to_string(),
                    ));
                }
                s.to_string()
            }
        }
        Some(_) => return Err(AppError::BadRequest("api_key must be a string".to_string())),
    };

    let mut doc = json!({
        "provider": provider,
        "api_url": api_url,
        "api_key": api_key,
        "from_address": from_address,
        "from_name": from_name,
        "updated_at": chrono::Utc::now().to_rfc3339(),
    });
    let updated_by = admin_email(&state.db, &claims.sub).await;
    if let Some(obj) = doc.as_object_mut() {
        // An empty field is "not configured here", not an empty string in the row: leave it out so
        // the environment fallback is what a reader sees, and so the row reads honestly.
        for key in ["api_url", "api_key", "from_address", "from_name"] {
            let empty = obj
                .get(key)
                .and_then(|v| v.as_str())
                .map(|v| v.is_empty())
                .unwrap_or(false);
            if empty {
                obj.remove(key);
            }
        }
        obj.insert("updated_by".to_string(), json!(updated_by));
    }
    platform::seal_secrets(&mut doc)?;

    sqlx::query(
        "INSERT INTO admin_settings (key, value, description, updated_at)
         VALUES ($1, $2::jsonb, $3, NOW())
         ON CONFLICT (key) DO UPDATE SET value = $2::jsonb, description = $3, updated_at = NOW()",
    )
    .bind(platform::CONFIG_KEY)
    .bind(&doc)
    .bind(platform::DESCRIPTION)
    .execute(&state.db)
    .await?;

    tracing::info!(
        provider = %doc.get("provider").and_then(|v| v.as_str()).unwrap_or(""),
        api_key_set = doc.get("api_key").is_some(),
        updated_by = %claims.sub,
        "platform mail transport saved from the admin panel"
    );

    let mut out = view(&state).await;
    if let Some(obj) = out.as_object_mut() {
        obj.insert("success".to_string(), json!(true));
    }
    Ok(Json(out))
}

/// DELETE /api/admin/email-config — drop the stored override and let the server environment carry
/// platform mail again. Reported with the state that is now in force.
pub async fn delete_config(State(state): State<AppState>) -> Result<Json<Value>, AppError> {
    let result = sqlx::query("DELETE FROM admin_settings WHERE key = $1")
        .bind(platform::CONFIG_KEY)
        .execute(&state.db)
        .await?;
    tracing::info!(
        removed = result.rows_affected(),
        "stored platform mail transport removed — the server environment carries platform mail"
    );
    let mut out = view(&state).await;
    if let Some(obj) = out.as_object_mut() {
        obj.insert("success".to_string(), json!(true));
        obj.insert("removed".to_string(), json!(result.rows_affected() > 0));
    }
    Ok(Json(out))
}

/// POST /api/admin/email-config/test — send one real message through the PLATFORM transport.
///
/// Deliberately not `load_delivery_config`: that resolves a workspace's own BYOK first, so it
/// could answer for a different transport than the one this panel is testing. Everything
/// downstream is the same code the worker uses, and the outcome is recorded through the app's
/// single recording policy into `outbound_messages` — so the panel shows the database's answer
/// (status + the provider's own error text), not this handler's opinion.
pub async fn test_config(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, AppError> {
    let Some(transport) = platform::resolved(&state.db).await else {
        return Ok(Json(json!({
            "success": false,
            "configured": false,
            "detail": "Platform email transport not configured — save a provider, send endpoint and API key here first (or set EMAIL_API_URL / EMAIL_API_KEY / EMAIL_FROM in the server environment)."
        })));
    };

    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let requested = body
        .as_ref()
        .and_then(|Json(v)| body_str(v, "to"))
        .filter(|t| !t.is_empty());
    let to = match requested {
        Some(to) => {
            if !to.contains('@') {
                return Err(AppError::BadRequest(format!(
                    "'{to}' is not an email address."
                )));
            }
            to
        }
        None => admin_email(&state.db, &claims.sub).await.ok_or_else(|| {
            AppError::BadRequest(
                "No destination: pass {\"to\":\"you@yourdomain.com\"} (this admin account has no email on file)."
                    .to_string(),
            )
        })?,
    };

    let subject = "CoreSwift CRM platform email test".to_string();
    let text = format!(
        "This is a test of the CoreSwift CRM platform mail transport.\n\n\
         Transport source: {}\nFrom: {}\nEndpoint: {}\n\n\
         If you received it, the platform credential in the admin panel works.\n\n- CoreSwift CRM\n",
        transport.origin.as_str(),
        transport.from,
        transport.url
    );
    // The test message is built inline here (it never touches `email::queue_outbound_message`), so
    // the app's own support address is put on it with the SAME helper the transactional path uses
    // (kanban t_71cc3ad8) — one wording, no drifting copy. The operator's test mail is the one
    // message they read after entering credentials, so it must carry what a real send carries.
    let text = crate::email::with_support_footer(&text, "").0;

    let msg_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO outbound_messages (id, tenant_id, channel, to_address, subject, body, status)
         VALUES ($1, $2, 'email', $3, $4, $5, 'queued')",
    )
    .bind(msg_id)
    .bind(tenant_id)
    .bind(&to)
    .bind(&subject)
    .bind(&text)
    .execute(&state.db)
    .await?;

    let cfg =
        providers::platform_test_config(tenant_id, msg_id, &to, &subject, &text, &transport.mail());
    let outcome = providers::deliver(&cfg).await;
    if let Err(e) = providers::record_attempt(&state.db, msg_id, &outcome).await {
        tracing::error!(msg = %msg_id, error = %e, "failed to record the platform test send");
    }

    // Read the row back: what the panel shows is what the database recorded, after the app's one
    // recording policy ran (sent / failed with the provider's own status / queued with backoff).
    let row: Option<(String, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT status, error_message, provider, message_id FROM outbound_messages WHERE id = $1",
    )
    .bind(msg_id)
    .fetch_optional(&state.db)
    .await?;
    let (status, error, provider, provider_message_id) = row.unwrap_or_default();
    let provider = provider.or_else(|| outcome.provider.clone());
    let detail = if outcome.ok {
        format!(
            "{} accepted the message",
            provider.as_deref().unwrap_or("the provider")
        )
    } else {
        error
            .clone()
            .unwrap_or_else(|| "the provider refused the message".to_string())
    };

    Ok(Json(json!({
        "success": outcome.ok,
        "configured": true,
        "transport_source": transport.origin.as_str(),
        "from": transport.from,
        "provider": provider,
        "to": to,
        "message_id": msg_id,
        "status": status,
        "error": error,
        "provider_message_id": provider_message_id,
        "detail": detail,
    })))
}
