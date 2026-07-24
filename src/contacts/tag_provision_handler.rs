use axum::{extract::State, http::HeaderMap, Json};
use axum::response::IntoResponse;
use uuid::Uuid;
use chrono::Utc;

use crate::AppState;
use crate::errors::{ApiResult, AppError};

/// POST /api/v1/internal/tag-provision
/// FunnelSwift tag assignment → create a contact in CoreSwift CRM
pub async fn tag_provision(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<serde_json::Value>,
) -> ApiResult<impl IntoResponse> {
    // Validate internal key
    let key = headers.get("x-internal-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if key != s.config.internal_sync_key {
        return Err(AppError::Unauthorized);
    }

    // Extract contact info from FunnelSwift payload
    let contact = payload.get("contact");
    let tag = payload.get("tag");

    let email = contact
        .and_then(|c| c.get("email"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if email.is_empty() {
        return Err(AppError::BadRequest("contact.email is required".into()));
    }

    let first_name = contact
        .and_then(|c| c.get("first_name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let last_name = contact
        .and_then(|c| c.get("last_name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let phone = contact
        .and_then(|c| c.get("phone"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let company = contact
        .and_then(|c| c.get("company"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let tag_name = tag
        .and_then(|t| t.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    // Idempotency check — skip if contact with this email already exists
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM contacts WHERE email = $1 LIMIT 1"
    )
    .bind(&email)
    .fetch_optional(&s.db)
    .await?;

    if let Some((existing_id,)) = existing {
        return Ok(Json(serde_json::json!({
            "status": "exists",
            "contact_id": existing_id.to_string(),
            "message": format!("Contact with email {} already exists", email)
        })));
    }

    // Find the system tenant (or create contact for Swift tenant)
    let tenant_id: Uuid = Uuid::parse_str("095d25ca-b5d2-485a-80df-8964d94642d6")
        .map_err(|_| AppError::Internal("Invalid system tenant UUID".into()))?;

    let contact_id = Uuid::new_v4();
    let now = Utc::now().naive_utc();

    sqlx::query(
        "INSERT INTO contacts (id, tenant_id, first_name, last_name, email, phone, company_name, notes, created_at, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
    )
    .bind(contact_id)
    .bind(tenant_id)
    .bind(&first_name)
    .bind(&last_name)
    .bind(&email)
    .bind(&phone)
    .bind(&company)
    .bind(format!("Auto-provisioned from FunnelSwift tag: {}", tag_name))
    .bind(now)
    .bind(now)
    .execute(&s.db)
    .await?;

    tracing::info!(
        contact_id = %contact_id,
        email = %email,
        tag = %tag_name,
        "tag_provision: CoreSwift CRM contact created"
    );

    Ok(Json(serde_json::json!({
        "status": "created",
        "contact_id": contact_id.to_string(),
        "email": email,
        "message": format!("Contact created via tag: {}", tag_name)
    })))
}
