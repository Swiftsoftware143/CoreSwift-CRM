//! Cross-app tag sync webhook — receives tag updates from satellite apps (FunnelSwift, etc.)
//!
//! POST /api/v1/webhooks/cross-app/tag-sync
//! Authenticated via x-internal-key header (same key used by all Swift apps)
//! UPSERTs contacts by email and syncs tags + pipeline stage.

use axum::response::IntoResponse;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::errors::{ApiResult, AppError};
use crate::AppState;

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
    // The shared internal secret is REQUIRED. This route writes into a tenant and, when the
    // tenant_id in the body is unknown, AUTO-CREATES that tenant — so an unauthenticated caller
    // could write into, or create, an arbitrary tenant. It is not a public route.
    //
    // "localhost trust" is NOT a boundary here and nothing implements one, because it cannot work:
    // every caller reaches this process from 127.0.0.1. The sibling services run with
    // network_mode: host and dial http://localhost:8084, and the public vhosts proxy /api/ to
    // 127.0.0.1:8084 as well — so a peer-address loopback test cannot tell a trusted producer from
    // an internet caller, and would have refused nothing. The secret is the boundary: every
    // producer (FunnelSwift, MissedCall, AdaSwift, WorkflowSwift, IncentiveSwift, multi-directory)
    // sends it as `x-internal-key`.
    //
    // config.rs defaults INTERNAL_SYNC_KEY to "", and an unset key would otherwise authenticate a
    // request whose header is empty; refuse when this server has no key configured (fail closed).
    let expected = s.config.internal_sync_key.as_str();
    let key = headers
        .get("x-internal-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // `internal-key` is the legacy spelling of the same header and is still accepted for
    // compatibility (it must equal the same configured secret; nothing but this file ever sent it).
    let legacy_key = headers
        .get("internal-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if expected.is_empty() || (key != expected && legacy_key != expected) {
        // Lengths only. This used to log the presented value AND the configured secret at WARN,
        // which is the one level routinely shipped to log aggregators.
        tracing::warn!(
            "TagSync webhook refused: invalid internal key (presented_len={}, configured_len={})",
            if key.is_empty() {
                legacy_key.len()
            } else {
                key.len()
            },
            expected.len()
        );
        return Err(AppError::Unauthorized);
    }

    // Parse tenant_id
    let tenant_id = Uuid::parse_str(&req.tenant_id)
        .map_err(|_| AppError::BadRequest("Invalid tenant_id".into()))?;

    // Verify tenant exists; auto-create from FunnelSwift sync if not
    let tenant_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tenants WHERE id = $1)")
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
            format!(
                "FS-{}",
                &tenant_source_name[..tenant_source_name.len().min(30)]
            )
        };

        let _ = sqlx::query(
            "INSERT INTO tenants (id, name, slug, created_at, updated_at) VALUES ($1, $2, $3, NOW(), NOW())"
        )
        .bind(tenant_id)
        .bind(&tenant_name)
        .bind(tenant_id.to_string())
        .execute(&s.db)
        .await;

        tracing::info!(
            "TagSync: Auto-created tenant {} ({})",
            tenant_id,
            tenant_name
        );
    }

    let email = req.lead.email.trim().to_lowercase();
    let name = req.lead.name.trim().to_string();
    let company = req.lead.company.as_deref().unwrap_or("").trim().to_string();

    if email.is_empty() && name.is_empty() {
        return Err(AppError::BadRequest(
            "Lead must have at least an email or name".into(),
        ));
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
            tracing::info!(
                "TagSync: Found existing contact {} ({} {}) in tenant {}",
                contact_id,
                first,
                last,
                tenant_id
            );
        } else {
            // Create new contact.
            // idx_contacts_tenant_email (partial, WHERE email IS NOT NULL) makes (tenant_id, email)
            // unique, so N concurrent syncs carrying the SAME lead used to raise 23505 here: the `?`
            // propagated AppError::Database -> 500 and the rest of the request (tag, list membership,
            // pipeline stage) was dropped. DO NOTHING absorbs the race and the re-select returns
            // whichever row won, so this stays a get-or-create.
            let (first_name, last_name) = split_name(&name);
            let mut created: Option<Uuid> = None;
            let mut adopted: Option<(Uuid, bool)> = None;
            for _ in 0..2 {
                let candidate = Uuid::new_v4();
                let inserted = sqlx::query(
                    r#"INSERT INTO contacts (id, tenant_id, first_name, last_name, email, company, source, created_at, updated_at)
                       VALUES ($1, $2, $3, $4, $5, $6, $7, NOW(), NOW())
                       ON CONFLICT (tenant_id, email) WHERE email IS NOT NULL DO NOTHING"#
                )
                .bind(candidate)
                .bind(tenant_id)
                .bind(&first_name)
                .bind(&last_name)
                .bind(&email)
                .bind(&company)
                .bind(format!("funnelswift:{}", req.source_app))
                .execute(&s.db)
                .await?;

                if inserted.rows_affected() == 1 {
                    created = Some(candidate);
                    break;
                }

                // Lost the race: read the winner by the exact key the unique index enforces.
                let winner: Option<(Uuid, bool)> = sqlx::query_as(
                    // is_active is NULLABLE with DEFAULT true; the consumer only asks "was the
                    // winner soft-deleted?", so COALESCE names the schema default (t_d6eeea96)
                    "SELECT id, COALESCE(is_active, true) FROM contacts WHERE tenant_id = $1 AND email = $2 LIMIT 1"
                )
                .bind(tenant_id)
                .bind(&email)
                .fetch_optional(&s.db)
                .await?;

                if let Some(found) = winner {
                    adopted = Some(found);
                    break;
                }
                // The winner vanished before we could read it; the loop retries the insert once.
            }

            if let Some(id) = created {
                contact_id = id;
                is_new = true;
                tracing::info!(
                    "TagSync: Created new contact {} ({} {}) in tenant {}",
                    contact_id,
                    first_name,
                    last_name,
                    tenant_id
                );
            } else if let Some((id, active)) = adopted {
                contact_id = id;
                is_new = false;
                if active {
                    tracing::info!(
                        "TagSync: adopted concurrently-created contact {} in tenant {}",
                        contact_id,
                        tenant_id
                    );
                } else {
                    // The lookup above filters is_active, the unique index does not: a soft-deleted
                    // contact owns this (tenant_id, email), so its id must be reused rather than
                    // 500-ing. Warned so the state is observable instead of silent.
                    tracing::warn!(
                        "TagSync: reusing INACTIVE contact {} for tenant {} (unique (tenant_id,email))",
                        contact_id,
                        tenant_id
                    );
                }
            } else {
                return Err(AppError::Internal(format!(
                    "TagSync: no contacts row for tenant {} after 2 insert attempts",
                    tenant_id
                )));
            }
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
            .bind(format!("funnelswift:{}", req.source_app))
            .execute(&s.db)
            .await?;
            is_new = true;
        }
    }

    // Sync tags: ensure each tag exists in CoreSwift, then assign.
    // A synced tag IS a tag on this tenant's contact, so the assignment fans out to the tenant's
    // `TagAdded` automations — FunnelSwift (the fleet's capture app) delivers its leads here, and
    // a tenant that automates on a tag must not have to care which surface carried it
    // (kanban t_56dddec2). Gated on `rows_affected()`: the statement is `ON CONFLICT DO NOTHING`,
    // so a repeat sync of unchanged tags writes nothing and fires nothing.
    for tag_name in &req.tags {
        // Create tag if it doesn't exist
        let tag_id = create_or_get_tag(&s.db, tenant_id, tag_name).await?;

        // Assign tag to contact.
        // `.ok()` keeps this path's pre-existing behaviour that a failed tag write does not fail
        // the whole sync (it was `let _ =` before the fan-out was added); the fan-out still only
        // happens when a row really appeared.
        let assigned = sqlx::query(
            "INSERT INTO tag_assignments (id, tag_id, entity_type, entity_id, tenant_id) VALUES ($1, $2, 'contact', $3, $4) ON CONFLICT (tag_id, entity_type, entity_id, tenant_id) DO NOTHING"
        )
        .bind(Uuid::new_v4())
        .bind(tag_id)
        .bind(contact_id)
        .bind(tenant_id)
        .execute(&s.db)
        .await
        .ok();
        if assigned.map(|r| r.rows_affected()).unwrap_or(0) > 0 {
            crate::automation::engine::fire_tag_trigger(
                &s.db, tenant_id, "contact", contact_id, tag_id, "TagAdded",
            )
            .await;
        }
    }

    // Remove tags that were removed. Same rule in the other direction: `TagRemoved` <=> a row
    // actually went away, so a repeat sync (`removed_tags` still listing an already-removed tag)
    // deletes nothing and fires nothing.
    for tag_name in &req.removed_tags {
        if let Some(tag_id) = get_tag_id_by_name(&s.db, tenant_id, tag_name).await {
            let removed = sqlx::query(
                "DELETE FROM tag_assignments WHERE tag_id = $1 AND entity_type = 'contact' AND entity_id = $2 AND tenant_id = $3"
            )
            .bind(tag_id)
            .bind(contact_id)
            .bind(tenant_id)
            .execute(&s.db)
            .await
            .ok();
            if removed.map(|r| r.rows_affected()).unwrap_or(0) > 0 {
                crate::automation::engine::fire_tag_trigger(
                    &s.db,
                    tenant_id,
                    "contact",
                    contact_id,
                    tag_id,
                    "TagRemoved",
                )
                .await;
            }
        }
    }

    // Add contact to a "FunnelSwift Leads" list (create if needed)
    // Assign to software-specific list (e.g., "FunnelSwift Clients", "MissedCall Clients")
    let list_name = format!("{} Clients", capitalize_source(req.source_app.as_str()));
    let list_id = create_or_get_list(&s.db, tenant_id, list_name.as_str()).await?;

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
        contact_id,
        tenant_id,
        req.tags,
        req.added_tags,
        req.removed_tags,
        req.triggered_by
    );

    Ok((
        StatusCode::OK,
        Json(json!({
            "status": "synced",
            "contact_id": contact_id.to_string(),
            "is_new": is_new,
            "tenant_id": tenant_id.to_string(),
            "tags_synced": req.tags.len(),
            "pipeline_stage": pipeline_stage,
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
            .await
            .map_err(AppError::Database)?;

    if let Some((id,)) = existing {
        return Ok(id);
    }

    // idx_tags_name_tenant makes (tenant_id, name) unique, so two concurrent get-or-creates could
    // raise 23505 here -> AppError::Database -> a 500 that ALSO dropped the rest of the request (the
    // tag assignment, list membership and pipeline stage). DO NOTHING turns that race into a no-op
    // and the re-select below returns whichever row won, keeping this a get-or-create.
    for _ in 0..2 {
        let id = Uuid::new_v4();
        let inserted = sqlx::query(
            "INSERT INTO tags (id, tenant_id, name, color, is_active) VALUES ($1, $2, $3, $4, true) ON CONFLICT (tenant_id, name) DO NOTHING"
        )
        .bind(id)
        .bind(tenant_id)
        .bind(tag_name)
        .bind(default_color(tag_name))
        .execute(db)
        .await
        .map_err(AppError::Database)?;

        if inserted.rows_affected() == 1 {
            return Ok(id);
        }

        // Lost the race: a peer committed the row between our SELECT and this INSERT.
        let winner: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM tags WHERE tenant_id = $1 AND name = $2 LIMIT 1")
                .bind(tenant_id)
                .bind(tag_name)
                .fetch_optional(db)
                .await
                .map_err(AppError::Database)?;

        if let Some((winner_id,)) = winner {
            tracing::info!(
                "TagSync: adopted concurrently-created tag {} ('{}') in tenant {}",
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

/// Get a tag ID by name
async fn get_tag_id_by_name(db: &sqlx::PgPool, tenant_id: Uuid, tag_name: &str) -> Option<Uuid> {
    sqlx::query_scalar::<_, Uuid>("SELECT id FROM tags WHERE tenant_id = $1 AND name = $2")
        .bind(tenant_id)
        .bind(tag_name)
        .fetch_optional(db)
        .await
        .unwrap_or(None)
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
    .await
    .map_err(AppError::Database)?;

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
        .bind("Auto-created by FunnelSwift tag sync")
        .execute(db)
        .await
        .map_err(AppError::Database)?;
        let (id,): (Uuid,) = sqlx::query_as(
            "SELECT id FROM lists WHERE tenant_id = $1 AND name = $2 ORDER BY (list_type = 'static') DESC, created_at LIMIT 1",
        )
        .bind(tenant_id)
        .bind(list_name)
        .fetch_one(db)
        .await
        .map_err(AppError::Database)?;
        Ok(id)
    }
}

/// Split a full name into first and last
fn split_name(full_name: &str) -> (String, String) {
    let trimmed = full_name.trim();
    if let Some(space) = trimmed.find(' ') {
        let first = trimmed[..space].trim().to_string();
        let last = trimmed[space + 1..].trim().to_string();
        (
            if first.is_empty() {
                trimmed.to_string()
            } else {
                first
            },
            if last.is_empty() {
                "Unknown".to_string()
            } else {
                last
            },
        )
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
    axum::Router::new().route("/cross-app/tag-sync", post(handle_tag_sync))
}

/// Capitalize source app name for display (e.g., "funnelswift" → "FunnelSwift")
fn capitalize_source(source: &str) -> String {
    match source {
        "funnelswift" => "FunnelSwift".into(),
        "missedcallrespondr" => "MissedCall".into(),
        "incentiveswift" => "IncentiveSwift".into(),
        "workflowswift" => "WorkflowSwift".into(),
        "coreswift" => "CoreSwift".into(),
        "adaswift" => "ADASwift".into(),
        _ => {
            let mut chars = source.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => c.to_uppercase().to_string() + chars.as_str(),
            }
        }
    }
}
