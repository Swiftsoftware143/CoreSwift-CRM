//! Tag provision webhook — receives FunnelSwift system tag assignments
//! and auto-provisions a free-tier tenant/contact in CoreSwift CRM.
//!
//! POST /api/v1/internal/tag-provision
//! Protected by X-Internal-Key header matching INTERNAL_SYNC_KEY env var.

use axum::response::IntoResponse;
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::Response,
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::signup::{self, MintRequest};
use crate::errors::{ApiResult, AppError};
use crate::security::email_addr;
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

    // Harness provenance (kanban t_3492e3d9). This route MINTS a tenant out of arbitrary caller
    // text, so a call made by a verification harness would otherwise leave a root that is
    // indistinguishable from a customer's on every sweep arm. Same helper as the public signup
    // path (one validator, no second copy of the rule); NULL when the header is absent or invalid,
    // which is exactly the pre-existing behaviour, and the column is read by nothing.
    let probe_harness = crate::auth::handlers::harness_marker(&headers);
    // The mint and its plan row commit TOGETHER (kanban t_6f225dd4). A tenant that lands with no
    // `tenant_plans` row resolves through `module_registry::resolve`'s `no_plan` arm — every
    // registered module granted and NO numeric ceiling — so this route minted an UNLIMITED workspace
    // per call. It now seats the same platform default (`free`) a signup gets; the capture below is
    // unaffected, because this arm writes its contact with its own `INSERT INTO contacts` further
    // down and consults no ceiling (t_e2364c41).
    let mut tx = s.db.begin().await?;
    let created: Option<(Uuid,)> = sqlx::query_as(
        r#"INSERT INTO tenants (id, name, slug, probe_harness, created_at, updated_at)
           VALUES ($1, $2, $3, $4, NOW(), NOW())
           ON CONFLICT (id) DO NOTHING
           RETURNING id"#,
    )
    .bind(tenant_id)
    .bind(&tenant_name)
    .bind(tenant_id.to_string())
    .bind(&probe_harness)
    .fetch_optional(&mut *tx)
    .await?;
    if created.is_some() {
        crate::billing::seat_default_plan(&mut tx, tenant_id).await?;
    }
    tx.commit().await?;

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

    // 6. Assign a "Free" tag to the contact.
    // DELIBERATELY NO TAG FAN-OUT (kanban t_56dddec2). Two reasons, both measured: (a) the tag
    // applied here is CoreSwift's OWN provisioning marker, hard-coded "Free" — the payload's
    // `tag.name` is only logged, never applied, so this is not a caller-requested tag operation;
    // (b) the tenant is created by THIS request (step 4, `Uuid::new_v4()`), so the assignment is
    // the new tenant's birth state and there is provably no rule it could fire (0 rows in
    // `automation_rules` for that tenant at that moment). The rule for every other writer is on
    // `crate::automation::engine::fire_tag_trigger`.
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

// ═══════════════════════════════════════════════════════════════════════════════════════════════
// POST /api/v1/internal/provision-free-account — the ACCOUNT door (kanban t_e968e9ad)
//
// The sibling door above captures a LEAD (tenant + contact). This one mints the account the lead
// holder can actually log into and upgrade in place: the SAME unit the self-serve signup mints —
// workspace + entry-plan row + owner user — through the ONE shared writer
// (`auth::signup::create_account`). Contract frozen in
// /opt/swift/docs/tag-to-free-account-design-2026-10-06.md §3.1.
// ═══════════════════════════════════════════════════════════════════════════════════════════════

/// `admin_settings` key: the per-app master switch. Ships ABSENT, which reads as `false`.
pub const PROVISION_ENABLED_KEY: &str = "provision_from_tags_enabled";
/// `admin_settings` key: which of THIS app's plans a tag-provisioned account is seated on.
pub const PROVISION_ENTRY_PLAN_KEY: &str = "provision_entry_plan_slug";
/// The entry-plan slug used when the setting is absent (the platform's own default plan).
pub const DEFAULT_ENTRY_PLAN_SLUG: &str = "free";

/// The provisioning knobs, as the app reads them.
#[derive(Debug, Clone)]
pub struct ProvisioningSettings {
    /// Master switch. `false` (the shipped state) makes the account door answer 403.
    pub enabled: bool,
    /// The plan slug a minted account is seated on. Resolved IN THIS APP — a sibling's plan name
    /// can never resolve here (spec §3.1 rule 1).
    pub entry_plan_slug: String,
}

/// Read both knobs. Absent keys, and values of an unexpected shape, fall back to the shipped
/// defaults — the door is ON by default; an operator turns it OFF from the console.
pub async fn read_provisioning_settings(
    db: &sqlx::PgPool,
) -> Result<ProvisioningSettings, AppError> {
    let enabled = read_setting(db, PROVISION_ENABLED_KEY).await?;
    let slug = read_setting(db, PROVISION_ENTRY_PLAN_KEY).await?;
    Ok(ProvisioningSettings {
        enabled: bool_setting(enabled.as_ref()).unwrap_or(true),
        entry_plan_slug: string_setting(slug.as_ref(), &["plan_slug", "slug", "value"])
            .unwrap_or_else(|| DEFAULT_ENTRY_PLAN_SLUG.to_string()),
    })
}

/// Persist both knobs (the admin console's only writer). `None` leaves a knob untouched, so the
/// toggle and the picker can be saved independently.
pub async fn save_provisioning_settings(
    db: &sqlx::PgPool,
    enabled: Option<bool>,
    entry_plan_slug: Option<&str>,
) -> Result<(), AppError> {
    if let Some(enabled) = enabled {
        write_setting(db, PROVISION_ENABLED_KEY, &Value::Bool(enabled)).await?;
    }
    if let Some(slug) = entry_plan_slug {
        write_setting(
            db,
            PROVISION_ENTRY_PLAN_KEY,
            &Value::String(slug.trim().to_string()),
        )
        .await?;
    }
    Ok(())
}

async fn read_setting(db: &sqlx::PgPool, key: &str) -> Result<Option<Value>, AppError> {
    let row: Option<(Value,)> = sqlx::query_as("SELECT value FROM admin_settings WHERE key = $1")
        .bind(key)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|r| r.0))
}

async fn write_setting(db: &sqlx::PgPool, key: &str, value: &Value) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO admin_settings (key, value, description, updated_at)
         VALUES ($1, $2::jsonb, $3, NOW())
         ON CONFLICT (key) DO UPDATE SET value = $2::jsonb, updated_at = NOW()",
    )
    .bind(key)
    .bind(value.to_string())
    .bind("Tag-provisioned free accounts (FunnelSwift)")
    .execute(db)
    .await?;
    Ok(())
}

/// A boolean setting stored as a scalar, or inside an object (`{"enabled": true}`), or as the
/// string an HTML form would send.
fn bool_setting(value: Option<&Value>) -> Option<bool> {
    match value? {
        Value::Bool(b) => Some(*b),
        Value::Number(n) => n.as_i64().map(|i| i != 0),
        Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "on" | "yes" => Some(true),
            "false" | "0" | "off" | "no" | "" => Some(false),
            _ => None,
        },
        Value::Object(o) => o.get("enabled").and_then(|v| v.as_bool()),
        _ => None,
    }
}

/// A string setting stored as a scalar, or inside an object under one of `keys`.
fn string_setting(value: Option<&Value>, keys: &[&str]) -> Option<String> {
    let clean = |s: &str| {
        let t = s.trim();
        (!t.is_empty()).then(|| t.to_string())
    };
    match value? {
        Value::String(s) => clean(s),
        Value::Object(o) => keys
            .iter()
            .find_map(|k| o.get(*k).and_then(|v| v.as_str()).and_then(clean)),
        _ => None,
    }
}

/// Every plan of THIS app that may be used as an entry plan: active, and free.
pub async fn free_plans(db: &sqlx::PgPool) -> Result<Vec<(String, String)>, AppError> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT slug, name FROM plans WHERE is_active = true AND price_monthly = 0 ORDER BY sort_order, name",
    )
    .fetch_all(db)
    .await?;
    Ok(rows)
}

/// Payload of the account door (spec §3.1).
#[derive(Debug, Deserialize)]
pub struct ProvisionFreeAccountRequest {
    pub source: String,
    #[serde(default)]
    pub source_tenant_id: Option<String>,
    pub tag: ProvisionFreeAccountTag,
    pub contact: ProvisionFreeAccountContact,
    /// `<funnelswift lead uuid>:<app slug>`. Recorded in the log line; the idempotency itself is
    /// the app's own (`users_email_key` is global — one address, one login, forever).
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProvisionFreeAccountTag {
    pub name: String,
    /// The caller's SUGGESTION. Deliberately not used to resolve the plan: the entry plan is read
    /// from THIS app's `provision_entry_plan_slug` (spec §3.1 rule 1).
    #[serde(default)]
    pub plan_slug: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProvisionFreeAccountContact {
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
    #[serde(default)]
    pub company: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
}

/// POST /api/v1/internal/provision-free-account
///
/// 201 `provisioned` · 200 `already_exists` · 403 `refused` · 422 unusable address / no free plan.
pub async fn handle_provision_free_account(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<ProvisionFreeAccountRequest>,
) -> ApiResult<Response> {
    // 1. The shared service credential, fail closed exactly as the sibling door above: an app with
    //    no configured key must never authenticate an empty header. The credential is never logged.
    let key = headers
        .get("x-internal-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = s.config.internal_sync_key.as_str();
    if expected.is_empty() || key != expected {
        tracing::warn!(
            "provision_free_account: invalid internal key (presented_len={}, configured_len={})",
            key.len(),
            expected.len()
        );
        return Err(AppError::Unauthorized);
    }

    // 2. The master switch. Ships ON (code default true, so a FRESH INSTALL has the door open);
    //    an operator closes it from the console and this app then refuses and mints NOTHING, so a
    //    caller cannot create an account in an app whose operator has disabled the door.
    let settings = read_provisioning_settings(&s.db).await?;
    if !settings.enabled {
        tracing::info!(
            "provision_free_account: refused (provisioning disabled) tag={} source={}",
            req.tag.name,
            req.source
        );
        return Ok((
            StatusCode::FORBIDDEN,
            Json(json!({ "status": "refused", "reason": "provisioning_disabled" })),
        )
            .into_response());
    }

    // 3. The address. `users.email` is the login identity AND the only address credentials can
    //    reach, so an empty, malformed or placeholder address is refused (422) rather than minted.
    let email = email_addr::normalize(req.contact.email.as_deref().unwrap_or(""))
        .map_err(AppError::Validation)?;
    if is_placeholder_address(&email) {
        return Err(AppError::Validation(format!(
            "contact.email '{email}' is a placeholder address — refusing to mint an account"
        )));
    }

    // 4. The entry plan, resolved IN THIS APP and required to be a free, active plan.
    let entry_plan_slug = settings.entry_plan_slug.clone();
    if signup::entry_plan_id(&s.db, &entry_plan_slug)
        .await?
        .is_none()
    {
        return Err(AppError::Validation(format!(
            "no free plan '{entry_plan_slug}' is configured in this app \
             (a plan with that slug, is_active = true and price_monthly = 0 is required)"
        )));
    }

    // 5. Idempotency: one address, one login, forever (`users_email_key` is a GLOBAL unique
    //    constraint and `auth::handlers::login` resolves a user by address alone).
    if let Some((_, tenant_id)) = sqlx::query_as::<_, (Uuid, Uuid)>(
        "SELECT id, tenant_id FROM users WHERE lower(email) = $1 LIMIT 1",
    )
    .bind(&email)
    .fetch_optional(&s.db)
    .await?
    {
        tracing::info!(
            "provision_free_account: already_exists email={} tenant={}",
            email,
            tenant_id
        );
        return Ok((
            StatusCode::OK,
            Json(json!({
                "status": "already_exists",
                "account_id": tenant_id.to_string(),
                "login_email": email,
            })),
        )
            .into_response());
    }

    // 5b. Adoption. The sibling door above already minted a workspace for this address (tenant +
    //     its free `tenant_plans` row + a contact) with NO user — so the account HOLDER exists and
    //     nobody can log in. When such a workspace is found, the owner is minted INTO it instead of
    //     a second workspace being created for the same person. The `NOT EXISTS (users)` guard is
    //     what keeps this narrow: a real customer's workspace always has a user, so only the
    //     machine-minted, user-less shape can ever be adopted.
    let adopted: Option<(Uuid,)> = sqlx::query_as(
        r#"SELECT c.tenant_id
             FROM contacts c
            WHERE lower(c.email) = $1
              AND c.is_active = true
              AND NOT EXISTS (SELECT 1 FROM users u WHERE u.tenant_id = c.tenant_id)
            ORDER BY c.created_at DESC
            LIMIT 1"#,
    )
    .bind(&email)
    .fetch_optional(&s.db)
    .await?;

    // 6. Mint: the SAME unit the self-serve signup mints, through the SAME writer.
    let first = req.contact.first_name.as_deref().unwrap_or("").trim();
    let last = req.contact.last_name.as_deref().unwrap_or("").trim();
    let company = req.contact.company.as_deref().unwrap_or("").trim();
    let mut full_name = format!("{first} {last}").trim().to_string();
    if full_name.is_empty() {
        full_name = if company.is_empty() {
            email
                .split('@')
                .next()
                .unwrap_or("Account Holder")
                .to_string()
        } else {
            company.to_string()
        };
    }

    let harness = crate::auth::handlers::harness_marker(&headers);
    let password = generate_password();

    let mut tx = s.db.begin().await?;
    let account = signup::create_account(
        &s.db,
        &mut tx,
        MintRequest {
            email: &email,
            name: &full_name,
            password: &password,
            // Auto-named "<name>'s Workspace", exactly as a self-serve signup for this person
            // would be named.
            account_name: None,
            account_slug: None,
            invite_token: None,
            into_tenant: adopted.map(|(tenant_id,)| tenant_id),
            entry_plan_slug: &entry_plan_slug,
            harness: harness.as_deref(),
        },
    )
    .await?;
    tx.commit().await?;

    tracing::info!(
        "provision_free_account: provisioned tenant={} user={} email={} plan={} adopted={} source={} idem={}",
        account.tenant_id,
        account.user.id,
        email,
        entry_plan_slug,
        adopted.is_some(),
        req.source,
        req.idempotency_key.as_deref().unwrap_or("-")
    );

    // 7. The credentials mail — the app's existing template, the same one the signup door sends.
    //    The business never types a password on this path; it arrives in this message.
    {
        let db = s.db.clone();
        let tenant_id = account.tenant_id;
        let tenant_name = account.tenant_name.clone();
        let email = email.clone();
        let name = full_name.clone();
        tokio::spawn(async move {
            signup::send_credentials_email(&db, tenant_id, &tenant_name, &email, &name, &password)
                .await;
        });
    }

    Ok((
        StatusCode::CREATED,
        Json(json!({
            "status": "provisioned",
            "account_id": account.tenant_id.to_string(),
            "plan_slug": entry_plan_slug,
            "login_email": email,
        })),
    )
        .into_response())
}

/// An address a human could never receive credentials at — the shape the retired tag-path
/// fallback used (`fs-provision-<uuid>@placeholder.swift.local`). Spec §3.1 rule 5: never mint on
/// a placeholder address. `email_addr::normalize` has already rejected the empty/malformed cases.
fn is_placeholder_address(email: &str) -> bool {
    let Some((local, domain)) = email.rsplit_once('@') else {
        return true;
    };
    let local = local.to_ascii_lowercase();
    let domain = domain.to_ascii_lowercase();
    local.starts_with("fs-provision-")
        || local.starts_with("provision-")
        || domain.contains("placeholder")
        || domain.ends_with(".local")
        || domain == "localhost"
}

/// A server-generated password for a machine-minted account. 20 characters from an unambiguous
/// alphabet; `argon2`-hashed by the shared mint and mailed to the address above.
fn generate_password() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] =
        b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789!@#$%^&*-_=+";
    let mut rng = rand::thread_rng();
    (0..20)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}
