//! Contact handlers: CRUD + search with tenant isolation.

use axum::{
    extract::{Json, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::models::*;
use crate::audit;
use crate::auth::models::Claims;
use crate::errors::{validate_pagination, ApiResult, AppError};
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct ContactListParams {
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct ContactSearchParams {
    pub q: String,
    pub page: Option<i64>,
    pub per_page: Option<i64>,
}

/// GET /api/contacts — List contacts with pagination (tenant-scoped).
pub async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<ContactListParams>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let (page, per_page) = validate_pagination(params.page, params.per_page);
    let offset = (page - 1) * per_page;

    let contacts = sqlx::query_as::<_, Contact>(
        r#"SELECT * FROM contacts
           WHERE tenant_id = $1 AND is_active = true
           ORDER BY created_at DESC
           LIMIT $2 OFFSET $3"#,
    )
    .bind(account_id)
    .bind(per_page)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(
        json!({ "contacts": contacts, "page": page, "per_page": per_page }),
    ))
}

/// GET /api/contacts/search?q=... — Full-text search on contacts.
pub async fn search(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<ContactSearchParams>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    if params.q.is_empty() {
        return Err(AppError::Validation(
            "Search query 'q' is required".to_string(),
        ));
    }

    let (page, per_page) = validate_pagination(params.page, params.per_page);
    let offset = (page - 1) * per_page;
    let pattern = format!("%{}%", params.q);

    let contacts = sqlx::query_as::<_, Contact>(
        r#"SELECT * FROM contacts
           WHERE tenant_id = $1 AND is_active = true
           AND (first_name ILIKE $2 OR last_name ILIKE $2 OR email ILIKE $2 OR phone ILIKE $2)
           ORDER BY created_at DESC
           LIMIT $3 OFFSET $4"#,
    )
    .bind(account_id)
    .bind(&pattern)
    .bind(per_page)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(
        json!({ "contacts": contacts, "query": params.q, "page": page, "per_page": per_page }),
    ))
}

/// POST /api/contacts — Create a new contact (dedup by email).
pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(mut req): Json<CreateContactRequest>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // FREE-TEXT HYGIENE BEFORE THE WRITE (`security::free_text`). A contact pasted out of a
    // spreadsheet or an email carried a NUL byte into the INSERT, and Postgres answered it as
    // `500 {"code":500,"error":true,"message":"Database error"}` with nothing saved and no field
    // named. The same byte is stripped from every other text column on the row.
    req.first_name = crate::security::free_text::single_line(&req.first_name, "First name", 100)?;
    req.last_name = crate::security::free_text::single_line(&req.last_name, "Last name", 100)?;
    for field in [
        &mut req.email,
        &mut req.phone,
        &mut req.title,
        &mut req.company,
        &mut req.gender,
        &mut req.address_line1,
        &mut req.address_line2,
        &mut req.city,
        &mut req.state,
        &mut req.postal_code,
        &mut req.country,
        &mut req.notes,
    ] {
        if let Some(value) = field.as_mut() {
            *value = crate::security::free_text::strip_nul(value);
        }
    }

    if req.first_name.is_empty() || req.last_name.is_empty() {
        return Err(AppError::Validation(
            "First name and last name are required".to_string(),
        ));
    }

    // t_47698f73: `company_id` may only name a company of the caller's own tenant. Before this,
    // any client could POST a random uuid and have it stored forever: no FK, no validation, no
    // reader. The FK in migration 087 now makes a dangling value unstorable, and this makes the
    // answer a 404 naming the field rather than a 500 from the constraint — and covers the case
    // the FK cannot see, a real company that belongs to somebody else's tenant.
    if let Some(company_id) = req.company_id {
        if !super::company_of_tenant(&state.db, company_id, account_id).await? {
            return Err(AppError::NotFound(format!(
                "Company {} not found for this tenant",
                company_id
            )));
        }
    }

    // If email is provided, check for existing contact with same email for this tenant
    if let Some(ref email) = req.email {
        if !email.is_empty() {
            let existing = crate::account_match::engine::find_contact_by_email_exact(
                &state.db, account_id, email,
            )
            .await?;

            if let Some(existing_contact) = existing {
                // Merge: UPDATE existing with any NEW non-null fields from the request
                let merged = sqlx::query_as::<_, Contact>(
                    r#"UPDATE contacts SET
                        phone = COALESCE($1, phone),
                        first_name = COALESCE($2, first_name),
                        last_name = COALESCE($3, last_name),
                        title = COALESCE($4, title),
                        company_id = COALESCE($5, company_id),
                        gender = COALESCE($6, gender),
                        address_line1 = COALESCE($7, address_line1),
                        address_line2 = COALESCE($8, address_line2),
                        city = COALESCE($9, city),
                        state = COALESCE($10, state),
                        postal_code = COALESCE($11, postal_code),
                        country = COALESCE($12, country),
                        notes = COALESCE($13, notes),
                        metadata = COALESCE($14, metadata),
                        company = CASE WHEN $15 IS NULL THEN company ELSE NULLIF(btrim($15), '') END,
                        updated_at = NOW()
                       WHERE id = $16 AND tenant_id = $17
                       RETURNING *"#,
                )
                .bind(&req.phone)
                .bind(&req.first_name)
                .bind(&req.last_name)
                .bind(&req.title)
                .bind(req.company_id)
                .bind(&req.gender)
                .bind(&req.address_line1)
                .bind(&req.address_line2)
                .bind(&req.city)
                .bind(&req.state)
                .bind(&req.postal_code)
                .bind(&req.country)
                .bind(&req.notes)
                .bind(&req.metadata)
                .bind(&req.company)
                .bind(existing_contact.id)
                .bind(account_id)
                .fetch_one(&state.db)
                .await?;

                return Ok((StatusCode::OK, Json(json!(merged))));
            }
        }
    }

    // No existing contact found — INSERT as normal.
    //
    // Usage ceiling (`limit_max_contacts`, the admin-assignable limit feature of the `limits`
    // module). The guard sits HERE, on the arm that ADDS a row, and nowhere else: the merge/update
    // arm above is deliberately not gated, or a workspace already at its ceiling could not edit a
    // contact it owns (kanban t_f49e4299; the wedge this caused on FunnelSwift is t_8ccc8e6a). The
    // number checked is the same `COUNT(*)` `GET /api/auth/me/usage` reports.
    let contact_usage = crate::features::count_contacts(&state.db, account_id).await;
    crate::features::enforce_usage_limit(
        &state.db,
        account_id,
        "limit_max_contacts",
        "Contact",
        "contacts",
        contact_usage,
    )
    .await?;

    // `company` is the free-text employer the product displays (decision on t_fbb30c16); blank
    // values for company/email/phone are stored as NULL, not '', so "absent" has one
    // representation — idx_contacts_tenant_email is UNIQUE WHERE email IS NOT NULL, so a second
    // ''-email contact in one tenant would otherwise be a 500.
    let contact = sqlx::query_as::<_, Contact>(
        r#"INSERT INTO contacts (id, tenant_id, email, phone, first_name, last_name, title,
            company_id, gender, address_line1, address_line2, city, state, postal_code, country,
            notes, metadata, company)
           VALUES ($1, $2, NULLIF(btrim($3), ''), NULLIF(btrim($4), ''), $5, $6, $7, $8, $9, $10,
                   $11, $12, $13, $14, $15, $16, $17, NULLIF(btrim($18), ''))
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(account_id)
    .bind(&req.email)
    .bind(&req.phone)
    .bind(&req.first_name)
    .bind(&req.last_name)
    .bind(&req.title)
    .bind(req.company_id)
    .bind(&req.gender)
    .bind(&req.address_line1)
    .bind(&req.address_line2)
    .bind(&req.city)
    .bind(&req.state)
    .bind(&req.postal_code)
    .bind(&req.country)
    .bind(&req.notes)
    .bind(&req.metadata)
    .bind(&req.company)
    .fetch_one(&state.db)
    .await?;

    // The console/API contact door is the canonical creation path (kanban t_990c386e): a
    // `contact.created` rule ("instant new-lead follow-up", "welcome") has to see contacts made
    // here, which is where a customer's own people add them.
    crate::automation::scan::contact_created(state.db.clone(), account_id, contact.id);

    Ok((StatusCode::CREATED, Json(json!(contact))))
}

/// GET /api/contacts/{id} — Get a single contact.
pub async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let contact =
        sqlx::query_as::<_, Contact>("SELECT * FROM contacts WHERE id = $1 AND tenant_id = $2")
            .bind(id)
            .bind(account_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound(format!("Contact {} not found", id)))?;

    // t_97b46a98: a NULL `is_active` is decoded as `None` (see `Contact::is_active`) so this route
    // answers 200 instead of 500. It is still worth one log line: no HTTP path can store a NULL
    // today (PATCH writes `COALESCE($17, is_active)`), so a NULL here means the column was written
    // by hand — and such a row is invisible to list/search, which both filter `is_active = true`.
    if contact.is_active.is_none() {
        tracing::warn!(
            contact_id = %contact.id,
            tenant_id = %contact.tenant_id,
            "contacts.is_active is NULL in the database (t_97b46a98): reporting is_active = null"
        );
    }

    Ok(Json(json!(contact)))
}

/// PATCH /api/contacts/{id} — Update a contact.
pub async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateContactRequest>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // If email is being changed, check that no OTHER contact already has that email for this tenant
    if let Some(ref email) = req.email {
        if !email.is_empty() {
            let existing = sqlx::query_as::<_, Contact>(
                "SELECT * FROM contacts WHERE tenant_id = $1 AND email = $2 AND id != $3 LIMIT 1",
            )
            .bind(account_id)
            .bind(email)
            .bind(id)
            .fetch_optional(&state.db)
            .await?;

            if existing.is_some() {
                return Err(AppError::Duplicate(
                    "Another contact with this email already exists".to_string(),
                ));
            }
        }
    }

    // t_47698f73: same gate as `create` — a PATCH below binds `company_id` straight into the
    // UPDATE, so without this check a client could re-point an existing contact at a uuid that
    // names nothing. 404 for a company that is not the caller's, rather than a FK 500 or a
    // silently stored dangling reference.
    if let Some(company_id) = req.company_id {
        if !super::company_of_tenant(&state.db, company_id, account_id).await? {
            return Err(AppError::NotFound(format!(
                "Company {} not found for this tenant",
                company_id
            )));
        }
    }

    let existing_id = id;
    // `company` is the free-text employer the product displays (decision on t_fbb30c16). NULL =
    // not mentioned -> keep the stored value; a blank string = clear it -> NULL, so "no company"
    // has one representation. Same normalisation for email/phone here and in `create`: a stored
    // '' would be a live 500 under idx_contacts_tenant_email (UNIQUE ... WHERE email IS NOT NULL).
    let contact = sqlx::query_as::<_, Contact>(
        r#"UPDATE contacts SET
            email = CASE WHEN $1 IS NULL THEN email ELSE NULLIF(btrim($1), '') END,
            phone = CASE WHEN $2 IS NULL THEN phone ELSE NULLIF(btrim($2), '') END,
            first_name = COALESCE($3, first_name),
            last_name = COALESCE($4, last_name),
            title = COALESCE($5, title),
            company_id = COALESCE($6, company_id),
            gender = COALESCE($7, gender),
            address_line1 = COALESCE($8, address_line1),
            address_line2 = COALESCE($9, address_line2),
            city = COALESCE($10, city),
            state = COALESCE($11, state),
            postal_code = COALESCE($12, postal_code),
            country = COALESCE($13, country),
            notes = COALESCE($14, notes),
            metadata = COALESCE($15, metadata),
            company = CASE WHEN $16 IS NULL THEN company ELSE NULLIF(btrim($16), '') END,
            is_active = COALESCE($17, is_active),
            updated_at = NOW()
           WHERE id = $18 AND tenant_id = $19
           RETURNING *"#,
    )
    .bind(&req.email)
    .bind(&req.phone)
    .bind(&req.first_name)
    .bind(&req.last_name)
    .bind(&req.title)
    .bind(req.company_id)
    .bind(&req.gender)
    .bind(&req.address_line1)
    .bind(&req.address_line2)
    .bind(&req.city)
    .bind(&req.state)
    .bind(&req.postal_code)
    .bind(&req.country)
    .bind(&req.notes)
    .bind(&req.metadata)
    .bind(&req.company)
    .bind(req.is_active)
    .bind(id)
    .bind(account_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound(format!("Contact {} not found", id)))?;

    // Log audit event
    audit::logger::log_event(
        &state.db,
        account_id,
        Some(Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?),
        "contact.updated",
        "contact",
        Some(existing_id),
        Some(json!({"updated": true})),
        None,
    )
    .await;

    Ok(Json(json!(contact)))
}

/// DELETE /api/contacts/{id} — Hard delete contact.
///
/// Deletes the contact AND its polymorphic children in ONE transaction.
///
/// `tag_assignments.entity_id` is polymorphic (`entity_type` + `entity_id`), so a FK to `contacts` is
/// impossible on it — deleting a contact used to leave its tag assignments behind forever, and the tag
/// screens then counted the leftovers as members (kanban t_7a54cc6a: 139 of 157 rows pointed at a
/// deleted contact). `list_members.contact_id` *does* cascade, but it is removed here explicitly so
/// every child that names a contact is retired by the same statement set that retires the contact.
/// A `contacts_before_delete_cleanup` trigger (migration 224) does the same for any path that deletes a
/// contact without going through this handler; the explicit deletes keep the intent visible here.
pub async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let user_id = Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?;

    let mut tx = state.db.begin().await?;

    // WHICH contact went, read BEFORE the row does: once the row is gone there is nothing left to
    // name it in the trail, and "a contact was deleted" is worth much less than which one.
    let gone: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT first_name, last_name, email FROM contacts WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(account_id)
    .fetch_optional(&mut *tx)
    .await?;

    // The contact's assignments and list memberships go in the same transaction as the contact, so a
    // half-finished delete can never mint an orphan assignment (the class this card exists for).
    sqlx::query(
        "DELETE FROM tag_assignments WHERE tenant_id = $1 AND entity_type = 'contact' AND entity_id = $2",
    )
    .bind(account_id)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM list_members WHERE tenant_id = $1 AND contact_id = $2")
        .bind(account_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;

    let result = sqlx::query("DELETE FROM contacts WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(account_id)
        .execute(&mut *tx)
        .await?;

    if result.rows_affected() == 0 {
        // Roll back: without the contact row the child deletes were no-ops for a contact that was never
        // this tenant's, and nothing should be committed for a delete that matched nothing.
        tx.rollback().await?;
        return Err(AppError::NotFound(format!("Contact {} not found", id)));
    }

    tx.commit().await?;

    if let Some((first, last, email)) = gone {
        let name = format!("{first} {last}").trim().to_string();
        audit::logger::log_event(
            &state.db,
            account_id,
            Some(user_id),
            "contact.deleted",
            "contact",
            Some(id),
            Some(json!({"name": name, "email": email})),
            None,
        )
        .await;
    }

    Ok(Json(json!({"message": "Contact deleted successfully"})))
}
