//! Tag provision webhook — receives FunnelSwift system tag assignments
//! and auto-provisions a free-tier tenant/contact in CoreSwift CRM.
//!
//! POST /api/v1/internal/tag-provision
//! Protected by X-Internal-Key header matching INTERNAL_SYNC_KEY env var.

use axum::response::IntoResponse;
use axum::{extract::State, http::HeaderMap, Json};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// Payload received from FunnelSwift tag webhook
#[derive(Debug, Deserialize)]
pub struct TagProvisionRequest {
    pub contact: TagProvisionContact,
    pub tag: TagProvisionTag,
    pub source: String,
    pub timestamp: String,
}

#[derive(Debug, Deserialize)]
pub struct TagProvisionContact {
    pub id: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub company: Option<String>,
    pub custom_fields: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct TagProvisionTag {
    pub name: String,
    pub campaign_id: Option<String>,
    pub metadata: Option<Value>,
}

/// POST /api/v1/internal/tag-provision
/// Receives FunnelSwift tag webhook, validates internal key,
/// creates or looks up a tenant + contact record.
pub async fn handle_tag_provision(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TagProvisionRequest>,
) -> ApiResult<impl IntoResponse> {
    // 1. Validate internal key
    let key = headers
        .get("x-internal-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = s.config.internal_sync_key.as_str();
    // config.rs defaults INTERNAL_SYNC_KEY to "", and an unset key would then authenticate an
    // empty x-internal-key header. Refuse when this server has no key configured (fail closed).
    // Never log the credential: `expected` is the shared INTERNAL_SYNC_KEY for every Swift
    // service and WARN is the level that gets shipped to log aggregators. Report lengths only.
    if expected.is_empty() || key != expected {
        tracing::warn!(
            "tag_provision: invalid internal key (presented_len={}, configured_len={})",
            key.len(),
            expected.len()
        );
        return Err(AppError::Unauthorized);
    }

    let email = req
        .contact
        .email
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_lowercase();
    let first_name = req
        .contact
        .first_name
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();
    let last_name = req
        .contact
        .last_name
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();
    let company_name = req
        .contact
        .company
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();
    let phone = req
        .contact
        .phone
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_string();

    tracing::info!(
        "tag_provision: received for tag={} email={} first={} last={} company={}",
        req.tag.name,
        email,
        first_name,
        last_name,
        company_name
    );

    // 2. If no email, use a placeholder from the tag + timestamp
    let lookup_email = if email.is_empty() {
        format!("fs-provision-{}@placeholder.swift.local", Uuid::new_v4())
    } else {
        email.clone()
    };

    // 3. Check if contact already exists by email
    let existing_contact: Option<(Uuid, Uuid)> = sqlx::query_as(
        r#"SELECT c.id, c.tenant_id
           FROM contacts c
           WHERE LOWER(c.email) = $1 AND c.is_active = true
           LIMIT 1"#,
    )
    .bind(&lookup_email)
    .fetch_optional(&s.db)
    .await?;

    if let Some((contact_id, tenant_id)) = existing_contact {
        tracing::info!(
            "tag_provision: contact already exists id={} tenant_id={}",
            contact_id,
            tenant_id
        );
        return Ok((
            axum::http::StatusCode::OK,
            Json(json!({
                "status": "already_exists",
                "contact_id": contact_id.to_string(),
                "tenant_id": tenant_id.to_string(),
            })),
        ));
    }

    // 4. Create a new tenant for this provisioned contact
    let tenant_id = Uuid::new_v4();
    let tenant_name = if !company_name.is_empty() {
        format!("FS-{}", &company_name[..company_name.len().min(60)])
    } else if !first_name.is_empty() {
        format!("FS-{}", &first_name[..first_name.len().min(60)])
    } else {
        format!(
            "FS-Provisioned-{}",
            &lookup_email[..lookup_email.len().min(40)]
        )
    };

    let _ = sqlx::query(
        r#"INSERT INTO tenants (id, name, slug, created_at, updated_at)
           VALUES ($1, $2, $3, NOW(), NOW())
           ON CONFLICT (id) DO NOTHING"#,
    )
    .bind(tenant_id)
    .bind(&tenant_name)
    .bind(tenant_id.to_string())
    .execute(&s.db)
    .await;

    tracing::info!(
        "tag_provision: created tenant {} ({})",
        tenant_id,
        tenant_name
    );

    // 5. Create the contact record
    let contact_id = Uuid::new_v4();

    sqlx::query(
        r#"INSERT INTO contacts (id, tenant_id, first_name, last_name, email, phone, company, source, created_at, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, NOW(), NOW())"#
    )
    .bind(contact_id)
    .bind(tenant_id)
    .bind(if first_name.is_empty() { &tenant_name } else { &first_name })
    .bind(if last_name.is_empty() { "Provisioned" } else { &last_name })
    .bind(&lookup_email)
    .bind(if phone.is_empty() { None } else { Some(&phone) })
    .bind(if company_name.is_empty() { None } else { Some(&company_name) })
    .bind(format!("funnelswift:{}", req.source))
    .execute(&s.db)
    .await?;

    tracing::info!(
        "tag_provision: created contact {} ({} {}) in tenant {}",
        contact_id,
        first_name,
        last_name,
        tenant_id
    );

    // 6. Assign a "Free" tag to the contact
    let free_tag_id = create_or_get_tag(&s.db, tenant_id, "Free").await?;
    let _ = sqlx::query(
        r#"INSERT INTO tag_assignments (id, tag_id, entity_type, entity_id, tenant_id)
           VALUES ($1, $2, 'contact', $3, $4)
           ON CONFLICT (tag_id, entity_type, entity_id, tenant_id) DO NOTHING"#,
    )
    .bind(Uuid::new_v4())
    .bind(free_tag_id)
    .bind(contact_id)
    .bind(tenant_id)
    .execute(&s.db)
    .await;

    // 7. Add contact to "FunnelSwift Leads" list
    let list_name = "FunnelSwift Leads";
    let list_id = create_or_get_list(&s.db, tenant_id, list_name).await?;
    let _ = sqlx::query(
        r#"INSERT INTO list_members (id, list_id, contact_id, tenant_id)
           VALUES ($1, $2, $3, $4)
           ON CONFLICT (list_id, contact_id) DO NOTHING"#,
    )
    .bind(Uuid::new_v4())
    .bind(list_id)
    .bind(contact_id)
    .bind(tenant_id)
    .execute(&s.db)
    .await;

    // 8. Send welcome email for newly provisioned contacts
    {
        let db = s.db.clone();
        let tid = tenant_id;
        let email = lookup_email.clone();
        let full_name = format!("{} {}", first_name, last_name).trim().to_string();
        tokio::spawn(async move {
            let vars = serde_json::json!({
                "name": full_name,
                "email": email,
                "app_url": "https://app.coreswiftcrm.com",
                "account_name": "CoreSwift CRM"
            });
            if let Err(e) =
                crate::email::send_template_email(&db, tid, &email, "welcome", &vars).await
            {
                tracing::warn!("Failed to send CoreSwift welcome email to {}: {}", email, e);
            }
        });
    }

    Ok((
        axum::http::StatusCode::CREATED,
        Json(json!({
            "status": "provisioned",
            "contact_id": contact_id.to_string(),
            "tenant_id": tenant_id.to_string(),
            "tag_assigned": "Free",
        })),
    ))
}

/// Create a tag if it doesn't exist, return its ID
async fn create_or_get_tag(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
    tag_name: &str,
) -> Result<Uuid, AppError> {
    let existing: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM tags WHERE tenant_id = $1 AND name = $2")
            .bind(tenant_id)
            .bind(tag_name)
            .fetch_optional(db)
            .await?;

    if let Some((id,)) = existing {
        return Ok(id);
    }

    // idx_tags_name_tenant makes (tenant_id, name) unique, so two concurrent get-or-creates could
    // raise 23505 here -> AppError::Database -> 500 that also dropped the rest of the request.
    // DO NOTHING turns the race into a no-op and the re-select below returns whichever row won, so
    // this stays a get-or-create instead of erroring.
    for _ in 0..2 {
        let id = Uuid::new_v4();
        let inserted = sqlx::query(
            "INSERT INTO tags (id, tenant_id, name, color, is_active) VALUES ($1, $2, $3, $4, true) ON CONFLICT (tenant_id, name) DO NOTHING"
        )
        .bind(id)
        .bind(tenant_id)
        .bind(tag_name)
        .bind("#4CAF50")
        .execute(db)
        .await?;

        if inserted.rows_affected() == 1 {
            return Ok(id);
        }

        // Lost the race: a peer committed the row between our SELECT and this INSERT.
        let winner: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM tags WHERE tenant_id = $1 AND name = $2 LIMIT 1")
                .bind(tenant_id)
                .bind(tag_name)
                .fetch_optional(db)
                .await?;

        if let Some((winner_id,)) = winner {
            tracing::info!(
                "tag_provision: adopted concurrently-created tag {} ('{}') in tenant {}",
                winner_id,
                tag_name,
                tenant_id
            );
            return Ok(winner_id);
        }
        // The winner was deleted again before we could read it; the loop retries the insert once.
    }

    Err(AppError::Internal(format!(
        "create_or_get_tag: no tags row for tenant {} name '{}' after 2 attempts",
        tenant_id, tag_name
    )))
}

/// Create or get a list by name
async fn create_or_get_list(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
    list_name: &str,
) -> Result<Uuid, AppError> {
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM lists WHERE tenant_id = $1 AND name = $2 AND list_type = 'static'",
    )
    .bind(tenant_id)
    .bind(list_name)
    .fetch_optional(db)
    .await?;

    if let Some((id,)) = existing {
        Ok(id)
    } else {
        let id = Uuid::new_v4();
        // idx_lists_name_tenant (migration 086) makes (tenant_id, name) unique, so two concurrent
        // provisions used to be able to raise 23505 here; DO NOTHING turns that race into a no-op
        // and the re-select below returns whichever row won, keeping this a get-or-create.
        sqlx::query(
            "INSERT INTO lists (id, tenant_id, name, list_type, description) VALUES ($1, $2, $3, 'static', $4) ON CONFLICT (tenant_id, name) DO NOTHING"
        )
        .bind(id)
        .bind(tenant_id)
        .bind(list_name)
        .bind("Auto-created by FunnelSwift tag provision")
        .execute(db)
        .await?;
        let (id,): (Uuid,) = sqlx::query_as(
            "SELECT id FROM lists WHERE tenant_id = $1 AND name = $2 ORDER BY (list_type = 'static') DESC, created_at LIMIT 1",
        )
        .bind(tenant_id)
        .bind(list_name)
        .fetch_one(db)
        .await?;
        Ok(id)
    }
}
