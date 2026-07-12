use axum::{
    extract::{State, Path, Json, Extension},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::errors::{AppError, ApiResult};
use crate::auth::models::Claims;
use crate::audit;
use super::models::*;

pub async fn list(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let companies = sqlx::query_as::<_, Company>(
        "SELECT * FROM companies WHERE account_id = $1 AND is_active = true ORDER BY name"
    ).bind(account_id).fetch_all(&state.db).await?;
    Ok(Json(json!({ "companies": companies })))
}

pub async fn create(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateCompanyRequest>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    if req.name.is_empty() {
        return Err(AppError::Validation("Company name is required".to_string()));
    }
    let company = sqlx::query_as::<_, Company>(
        r#"INSERT INTO companies (id, tenant_id, name, domain, industry, size, phone,
            address_line1, address_line2, city, state, postal_code, country, website, notes, metadata)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
           RETURNING *"#
    ).bind(Uuid::new_v4()).bind(account_id).bind(&req.name).bind(&req.domain)
    .bind(&req.industry).bind(&req.size).bind(&req.phone)
    .bind(&req.address_line1).bind(&req.address_line2).bind(&req.city)
    .bind(&req.state).bind(&req.postal_code).bind(&req.country)
    .bind(&req.website).bind(&req.notes).bind(&req.metadata)
    .fetch_one(&state.db).await?;
    Ok((StatusCode::CREATED, Json(json!(company))))
}

pub async fn get(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let company = sqlx::query_as::<_, Company>(
        "SELECT * FROM companies WHERE id = $1 AND account_id = $2"
    ).bind(id).bind(account_id).fetch_optional(&state.db).await?
    .ok_or(AppError::NotFound(format!("Company {} not found", id)))?;
    Ok(Json(json!(company)))
}

pub async fn update(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateCompanyRequest>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let company = sqlx::query_as::<_, Company>(
        r#"UPDATE companies SET name = COALESCE($1, name), domain = COALESCE($2, domain),
            industry = COALESCE($3, industry), size = COALESCE($4, size),
            phone = COALESCE($5, phone), address_line1 = COALESCE($6, address_line1),
            address_line2 = COALESCE($7, address_line2), city = COALESCE($8, city),
            state = COALESCE($9, state), postal_code = COALESCE($10, postal_code),
            country = COALESCE($11, country), website = COALESCE($12, website),
            notes = COALESCE($13, notes), metadata = COALESCE($14, metadata),
            is_active = COALESCE($15, is_active), updated_at = NOW()
           WHERE id = $16 AND account_id = $17 RETURNING *"#
    ).bind(&req.name).bind(&req.domain).bind(&req.industry).bind(&req.size)
    .bind(&req.phone).bind(&req.address_line1).bind(&req.address_line2)
    .bind(&req.city).bind(&req.state).bind(&req.postal_code).bind(&req.country)
    .bind(&req.website).bind(&req.notes).bind(&req.metadata).bind(req.is_active)
    .bind(id).bind(account_id).fetch_optional(&state.db).await?
    .ok_or(AppError::NotFound(format!("Company {} not found", id)))?;

    // Log audit event
    audit::logger::log_event(
        &state.db,
        account_id,
        Some(Uuid::parse_str(&claims.sub).map_err(|_| AppError::Unauthorized)?),
        "company.updated",
        "company",
        Some(id),
        Some(json!({"updated": true})),
        None,
    ).await;

    Ok(Json(json!(company)))
}

pub async fn delete(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM companies WHERE id = $1 AND account_id = $2")
        .bind(id).bind(account_id).execute(&state.db).await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Company {} not found", id)));
    }
    Ok(Json(json!({"message": "Company deleted successfully"})))
}
