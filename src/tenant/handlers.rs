//! Tenant handlers — get/update the current user's own tenant.

use axum::{
    extract::{State, Extension, Json},
    response::IntoResponse,
};
use serde_json::json;
use uuid::Uuid;

use crate::AppState;
use crate::auth::models::Claims;
use crate::errors::{AppError, ApiResult};

/// Request body for updating the current user's tenant.
#[derive(Debug, serde::Deserialize)]
pub struct UpdateCurrentTenantRequest {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub logo_url: Option<String>,
    pub primary_color: Option<String>,
    pub accent_color: Option<String>,
    pub custom_domain: Option<String>,
    pub settings: Option<serde_json::Value>,
}

/// GET /api/tenants — Get the current user's tenant info.
pub async fn get_current_tenant(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    #[derive(sqlx::FromRow, serde::Serialize)]
    struct TenantRow {
        id: Uuid,
        name: String,
        slug: String,
        logo_url: Option<String>,
        primary_color: Option<String>,
        accent_color: Option<String>,
        custom_domain: Option<String>,
        settings: Option<serde_json::Value>,
        is_active: bool,
        created_at: chrono::DateTime<chrono::Utc>,
        updated_at: chrono::DateTime<chrono::Utc>,
    }

    let tenant = sqlx::query_as::<_, TenantRow>(
        "SELECT id, name, slug, logo_url, primary_color, accent_color, custom_domain, settings, is_active, created_at, updated_at FROM tenants WHERE id = $1"
    )
    .bind(tenant_id)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound("Tenant not found".to_string()))?;

    Ok(Json(json!(tenant)))
}

/// PUT /api/tenants — Update the current user's tenant.
pub async fn update_current_tenant(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<UpdateCurrentTenantRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Only owner/admin can update tenant settings
    if claims.role != "owner" && claims.role != "admin" {
        return Err(AppError::Forbidden);
    }

    sqlx::query(
        r#"UPDATE tenants SET
            name = COALESCE($2, name),
            slug = COALESCE($3, slug),
            logo_url = COALESCE($4, logo_url),
            primary_color = COALESCE($5, primary_color),
            accent_color = COALESCE($6, accent_color),
            custom_domain = COALESCE($7, custom_domain),
            settings = COALESCE($8, settings),
            updated_at = NOW()
           WHERE id = $1"#
    )
    .bind(tenant_id)
    .bind(&req.name)
    .bind(&req.slug)
    .bind(&req.logo_url)
    .bind(&req.primary_color)
    .bind(&req.accent_color)
    .bind(&req.custom_domain)
    .bind(&req.settings)
    .execute(&state.db)
    .await?;

    Ok(Json(json!({
        "message": "Tenant updated successfully",
        "tenant_id": tenant_id,
    })))
}
