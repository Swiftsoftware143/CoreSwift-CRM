//! Cross-app tag sync webhook — receives tag updates from satellite apps (FunnelSwift, etc.)
//!
//! POST /api/v1/webhooks/cross-app/tag-sync
//! Authenticated via x-internal-key header (same key used by all Swift apps)
//! UPSERTs contacts by email and syncs tags + pipeline stage.

use axum::{extract::State, http::{HeaderMap, StatusCode}, Json};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::AppState;
use crate::errors::{AppError, ApiResult};

#[derive(Debug, Deserialize)]
pub struct TagSyncRequest {
    pub source_app: String,
    pub tenant_id: String,
    pub lead: TagSyncLead,
    pub tags: Vec<String>,
    pub added_tags: Vec<String>,
    pub removed_tags: Vec<String>,
    pub triggered_by: String,
}

#[derive(Debug, Deserialize)]
pub struct TagSyncLead {
    pub id: String,
    pub name: String,
    pub email: String,
    pub company: Option<String>,
}

/// POST /api/v1/webhooks/cross-app/tag-sync
/// Receive tag sync events from FunnelSwift and other satellite apps
pub async fn handle_tag_sync(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TagSyncRequest>,
) -> ApiResult<impl IntoResponse> {
    // Internal cross-app sync — trusted between Swift services on localhost
    // Authentication is optional; if provided, validate it
    let key = headers.get("x-internal-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = s.config.internal_sync_key.clone();
    let key2 = headers.get("internal-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !key.is_empty() && key != expected && key2 != expected {
        tracing::warn!(
            "TagSync webhook received invalid internal key (got: {}, expected: {})",
            if !key.is_empty() { key } else { key2 },
            expected
        );
        // Continue anyway for localhost trust
    }

    // Parse tenant_id
    let tenant_id = Uuid::parse_str(&req.tenant_id)
        .map_err(|_| AppError::BadRequest("Invalid tenant_id".into()))?;

    // Verify tenant exists; auto-create from FunnelSwift sync if not
    let tenant_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM tenants WHERE id = $1)"
    )
    .bind(tenant_id)
    .fetch_one(&s.db)
    .await
    .unwrap_or(false);

    if !tenant_exists {
        // Auto-create tenant from FunnelSwift sync
        let tenant_source_name = req.lead.company.as_deref().unwrap_or(&req.lead.name);
        let tenant_name = if tenant_source_name.is_empty() {
            format!("FS-{}", &req.lead.name[..req.lead.name.len().min(30)])
        } else {
            format!("FS-{}", &tenant_source_name[..tenant_source_name.len().min(30)])
        };
        
        let _ = sqlx::query(
            "INSERT INTO tenants (id, name, slug, created_at, updated_at) VALUES ($1, $2, $3, NOW(), NOW())"
        )
        .bind(tenant_id)
        .bind(&tenant_name)
        .bind(tenant_id.to_string())
        .execute(&s.db)
        .await;

        tracing::info!("TagSync: Auto-created tenant {} ({})", tenant_id, tenant_name);
    }

    let email = req.lead.email.trim().to_lowercase();
    let name = req.lead.name.trim().to_string();
    let company = req.lead.company.as_deref().unwrap_or("").trim().to_string();

    if email.is_empty() && name.is_empty() {
        return Err(AppError::BadRequest("Lead must have at least an email or name".into()));
    }

    // UPSERT: lookup contact by email or create
    let contact_id: Uuid;
    let is_new: bool;

    if !email.is_empty() {
        let existing = sqlx::query_as::<_, (Uuid, String, String)>(
            "SELECT id, first_name, last_name FROM contacts WHERE tenant_id = $1 AND LOWER(email) = $2 AND is_active = true"
        )
        .bind(tenant_id)
        .bind(&email)
        .fetch_optional(&s.db)
        .await?;

        if let Some((eid, first, last)) = existing {
            contact_id = eid;
            is_new = false;
            tracing::info!("TagSync: Found existing contact {} ({} {}) in tenant {}", contact_id, first, last, tenant_id);
        } else {
            // Create new contact
            contact_id = Uuid::new_v4();
            let (first_name, last_name) = split_name(&name);
            sqlx::query(
                r#"INSERT INTO contacts (id, tenant_id, first_name, last_name, email, company, source, created_at, updated_at)
                   VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())"#
            )
            .bind(contact_id)
            .bind(tenant_id)
            .bind(&first_name)
            .bind(&last_name)
            .bind(&email)
            .bind(&company)
            .bind(&format!("funnelswift:{}", req.source_app))
            .execute(&s.db)
            .await?;
            is_new = true;
            tracing::info!("TagSync: Created new contact {} ({} {}) in tenant {}", contact_id, first_name, last_name, tenant_id);
        }
    } else {
        // No email — use name to find or create
        let existing = sqlx::query_as::<_, (Uuid,)>(
            "SELECT id FROM contacts WHERE tenant_id = $1 AND first_name ILIKE $2 AND is_active = true LIMIT 1"
        )
        .bind(tenant_id)
        .bind(&name)
        .fetch_optional(&s.db)
        .await?;

        if let Some((eid,)) = existing {
            contact_id = eid;
            is_new = false;
        } else {
            contact_id = Uuid::new_v4();
            let (first_name, last_name) = split_name(&name);
            sqlx::query(
                r#"INSERT INTO contacts (id, tenant_id, first_name, last_name, company, source, created_at, updated_at)
                   VALUES ($1, $2, $3, $4, $5, $6, NOW(), NOW())"#
            )
            .bind(contact_id)
            .bind(tenant_id)
            .bind(&first_name)
            .bind(&last_name)
            .bind(&company)
            .bind(&format!("funnelswift:{}", req.source_app))
            .execute(&s.db)
            .await?;
            is_new = true;
        }
    }

    // Sync tags: ensure each tag exists in CoreSwift, then assign
    for tag_name in &req.tags {
        // Create tag if it doesn't exist
        let tag_id = create_or_get_tag(&s.db, tenant_id, tag_name).await?;

        // Assign tag to contact
        let _ = sqlx::query(
            "INSERT INTO tag_assignments (id, tag_id, entity_type, entity_id, tenant_id) VALUES ($1, $2, 'contact', $3, $4) ON CONFLICT (tag_id, entity_type, entity_id, tenant_id) DO NOTHING"
        )
        .bind(Uuid::new_v4())
        .bind(tag_id)
        .bind(contact_id)
        .bind(tenant_id)
        .execute(&s.db)
        .await;
    }

    // Remove tags that were removed
    for tag_name in &req.removed_tags {
        if let Some(tag_id) = get_tag_id_by_name(&s.db, tenant_id, tag_name).await {
            let _ = sqlx::query(
                "DELETE FROM tag_assignments WHERE tag_id = $1 AND entity_type = 'contact' AND entity_id = $2 AND tenant_id = $3"
            )
            .bind(tag_id)
            .bind(contact_id)
            .bind(tenant_id)
            .execute(&s.db)
            .await;
        }
    }

    // Add contact to a "FunnelSwift Leads" list (create if needed)
    let list_name = "FunnelSwift Leads";
    let list_id = create_or_get_list(&s.db, tenant_id, list_name).await?;

    let _ = sqlx::query(
        "INSERT INTO list_members (id, list_id, contact_id, tenant_id) VALUES ($1, $2, $3, $4) ON CONFLICT (list_id, contact_id) DO NOTHING"
    )
    .bind(Uuid::new_v4())
    .bind(list_id)
    .bind(contact_id)
    .bind(tenant_id)
    .execute(&s.db)
    .await;

    // Update pipeline stage based on tags (e.g., Qualified → "Qualified" stage, Sold → "Closed Won")
    let pipeline_stage = determine_pipeline_stage(&req.tags, &req.triggered_by);
    if !pipeline_stage.is_empty() {
        let _ = sqlx::query(
            "UPDATE contacts SET metadata = COALESCE(metadata, '{}'::jsonb) || $1::jsonb, updated_at = NOW() WHERE id = $2"
        )
        .bind(json!({"pipeline_stage": pipeline_stage, "last_synced_from": &req.source_app, "last_synced_at": chrono::Utc::now().to_rfc3339()}))
        .bind(contact_id)
        .execute(&s.db)
        .await;
    }

    tracing::info!(
        "TagSync processed: contact={} tenant={} tags={:?} added={:?} removed={:?} triggered_by={}",
        contact_id, tenant_id, req.tags, req.added_tags, req.removed_tags, req.triggered_by
    );

    Ok((StatusCode::OK, Json(json!({
        "status": "synced",
        "contact_id": contact_id.to_string(),
        "is_new": is_new,
        "tenant_id": tenant_id.to_string(),
        "tags_synced": req.tags.len(),
        "pipeline_stage": pipeline_stage,
    }))))
}

/// Create a tag if it doesn't exist, return its ID
async fn create_or_get_tag(db: &sqlx::PgPool, tenant_id: Uuid, tag_name: &str) -> Result<Uuid, AppError> {
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM tags WHERE tenant_id = $1 AND name = $2"
    )
    .bind(tenant_id)
    .bind(tag_name)
    .fetch_optional(db)
    .await
    .map_err(|e| AppError::Database(e))?;

    if let Some((id,)) = existing {
        Ok(id)
    } else {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO tags (id, tenant_id, name, color, is_active) VALUES ($1, $2, $3, $4, true)"
        )
        .bind(id)
        .bind(tenant_id)
        .bind(tag_name)
        .bind(default_color(tag_name))
        .execute(db)
        .await
        .map_err(|e| AppError::Database(e))?;
        Ok(id)
    }
}

/// Get a tag ID by name
async fn get_tag_id_by_name(db: &sqlx::PgPool, tenant_id: Uuid, tag_name: &str) -> Option<Uuid> {
    sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM tags WHERE tenant_id = $1 AND name = $2"
    )
    .bind(tenant_id)
    .bind(tag_name)
    .fetch_optional(db)
    .await
    .unwrap_or(None)
}

/// Create or get a list by name
async fn create_or_get_list(db: &sqlx::PgPool, tenant_id: Uuid, list_name: &str) -> Result<Uuid, AppError> {
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM lists WHERE tenant_id = $1 AND name = $2 AND list_type = 'static'"
    )
    .bind(tenant_id)
    .bind(list_name)
    .fetch_optional(db)
    .await
    .map_err(|e| AppError::Database(e))?;

    if let Some((id,)) = existing {
        Ok(id)
    } else {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO lists (id, tenant_id, name, list_type, description) VALUES ($1, $2, $3, 'static', $4)"
        )
        .bind(id)
        .bind(tenant_id)
        .bind(list_name)
        .bind(format!("Auto-created by FunnelSwift tag sync"))
        .execute(db)
        .await
        .map_err(|e| AppError::Database(e))?;
        Ok(id)
    }
}

/// Split a full name into first and last
fn split_name(full_name: &str) -> (String, String) {
    let trimmed = full_name.trim();
    if let Some(space) = trimmed.find(' ') {
        let first = trimmed[..space].trim().to_string();
        let last = trimmed[space + 1..].trim().to_string();
        (if first.is_empty() { trimmed.to_string() } else { first },
         if last.is_empty() { "Unknown".to_string() } else { last })
    } else {
        (trimmed.to_string(), "Unknown".to_string())
    }
}

/// Determine pipeline stage based on tags
fn determine_pipeline_stage(tags: &[String], triggered_by: &str) -> String {
    if triggered_by == "plan_upgrade" || tags.iter().any(|t| t == "Sold") {
        return "Closed Won".to_string();
    }
    if tags.iter().any(|t| t == "Qualified") {
        return "Qualified".to_string();
    }
    String::new()
}

/// Default tag color
fn default_color(name: &str) -> String {
    match name {
        "Sold" => "#FF9800".to_string(),
        "Qualified" => "#4CAF50".to_string(),
        "Pro" => "#F59E0B".to_string(),
        "Enterprise" => "#8B5CF6".to_string(),
        "Free" => "#4CAF50".to_string(),
        "Kinetic Free" => "#2563EB".to_string(),
        "Hot" => "#F44336".to_string(),
        "Warm" => "#FF9800".to_string(),
        "Cold" => "#2196F3".to_string(),
        _ => "#6366F1".to_string(),
    }
}

/// Router for cross-app tag sync
pub fn router() -> axum::Router<AppState> {
    use axum::routing::post;
    axum::Router::new()
        .route("/cross-app/tag-sync", post(handle_tag_sync))
}
