//! Contact handlers: CRUD + search with tenant isolation.

use axum::{
    extract::{State, Path, Query, Json},
    http::StatusCode,
    response::IntoResponse,
    Extension,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::errors::{AppError, ApiResult, validate_pagination};
use crate::auth::models::Claims;
use crate::audit;
use super::models::*;

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
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    let (page, per_page) = validate_pagination(params.page, params.per_page);
    let offset = (page - 1) * per_page;

    let contacts = sqlx::query_as::<_, Contact>(
        r#"SELECT * FROM contacts
           WHERE tenant_id = $1 AND is_active = true
           ORDER BY created_at DESC
           LIMIT $2 OFFSET $3"#,
    )
    .bind(tenant_id)
    .bind(per_page)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({ "contacts": contacts, "page": page, "per_page": per_page })))
}

/// GET /api/contacts/search?q=... — Full-text search on contacts.
pub async fn search(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Query(params): Query<ContactSearchParams>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    if params.q.is_empty() {
        return Err(AppError::Validation("Search query 'q' is required".to_string()));
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
    .bind(tenant_id)
    .bind(&pattern)
    .bind(per_page)
    .bind(offset)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(json!({ "contacts": contacts, "query": params.q, "page": page, "per_page": per_page })))
}

/// POST /api/contacts — Create a new contact.
pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateContactRequest>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    if req.first_name.is_empty() || req.last_name.is_empty() {
        return Err(AppError::Validation("First name and last name are required".to_string()));
    }

    let contact = sqlx::query_as::<_, Contact>(
        r#"INSERT INTO contacts (id, tenant_id, email, phone, first_name, last_name, title,
            company_id, gender, address_line1, address_line2, city, state, postal_code, country,
            notes, metadata)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
           RETURNING *"#,
    )
    .bind(Uuid::new_v4())
    .bind(tenant_id)
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
    .fetch_one(&state.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!(contact))))
}

/// GET /api/contacts/{id} — Get a single contact.
pub async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    let contact = sqlx::query_as::<_, Contact>(
        "SELECT * FROM contacts WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound(format!("Contact {} not found", id)))?;

    Ok(Json(json!(contact)))
}

/// PATCH /api/contacts/{id} — Update a contact.
pub async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateContactRequest>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    let existing_id = id;
    let contact = sqlx::query_as::<_, Contact>(
        r#"UPDATE contacts SET
            email = COALESCE($1, email),
            phone = COALESCE($2, phone),
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
            is_active = COALESCE($16, is_active),
            updated_at = NOW()
           WHERE id = $17 AND tenant_id = $18
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
    .bind(req.is_active)
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound(format!("Contact {} not found", id)))?;

    // Log audit event
    audit::logger::log_event(
        &state.db,
        tenant_id,
        Some(Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?),
        "contact.updated",
        "contact",
        Some(existing_id),
        Some(json!({"updated": true})),
        None,
    ).await;

    Ok(Json(json!(contact)))
}

/// DELETE /api/contacts/{id} — Hard delete contact.
pub async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid)
        .map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query("DELETE FROM contacts WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tenant_id)
        .execute(&state.db)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Contact {} not found", id)));
    }

    Ok(Json(json!({"message": "Contact deleted successfully"})))
}
