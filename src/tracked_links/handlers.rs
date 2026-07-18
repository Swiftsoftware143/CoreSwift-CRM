use axum::{
    extract::{State, Path, Json, Extension, Query},
    http::{StatusCode, Uri, HeaderMap},
    response::{IntoResponse, Redirect},
};
use serde_json::json;
use uuid::Uuid;
use rand::Rng;

use crate::AppState;
use crate::errors::{AppError, ApiResult};
use crate::auth::models::Claims;
use super::models::*;

/// POST /api/tracked-links — create a tracked link
pub async fn create_tracked_link(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(r): Json<CreateTrackedLinkRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    // Validate tag exists in this tenant
    let tag_exists: bool = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM tags WHERE id=$1 AND tenant_id=$2 AND is_active=true"
    )
    .bind(r.tag_id)
    .bind(tenant_id)
    .fetch_one(&s.db)
    .await
    .unwrap_or(0)
        > 0;

    if !tag_exists {
        return Err(AppError::NotFound(format!("Tag {} not found or inactive", r.tag_id)));
    }

    // Generate unique slug
    let slug = generate_slug(12);

    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tracked_links(id, tenant_id, tag_id, slug, target_url) VALUES($1,$2,$3,$4,$5)"
    )
    .bind(id)
    .bind(tenant_id)
    .bind(r.tag_id)
    .bind(&slug)
    .bind(&r.target_url)
    .execute(&s.db)
    .await?;

    let tracking_url = format!("/track/{}", slug);

    Ok((
        StatusCode::CREATED,
        Json(json!(CreateTrackedLinkResponse {
            id,
            tag_id: r.tag_id,
            slug: slug.clone(),
            target_url: r.target_url,
            tracking_url,
        })),
    ))
}

/// GET /api/tracked-links — list all tracked links for tenant with click counts
pub async fn list_tracked_links(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let links = sqlx::query_as::<_, TrackedLink>(
        "SELECT * FROM tracked_links WHERE tenant_id=$1 ORDER BY created_at DESC"
    )
    .bind(tenant_id)
    .fetch_all(&s.db)
    .await?;

    // Build results with click counts and tag info
    let mut results: Vec<TrackedLinkWithClicks> = Vec::new();
    for link in &links {
        let click_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM link_clicks WHERE tracked_link_id=$1"
        )
        .bind(link.id)
        .fetch_one(&s.db)
        .await
        .unwrap_or(0);

        let tag_info: Option<(String, Option<String>)> = sqlx::query_as(
            "SELECT name, color FROM tags WHERE id=$1"
        )
        .bind(link.tag_id)
        .fetch_optional(&s.db)
        .await
        .ok()
        .flatten();

        let (tag_name, tag_color) = tag_info.unwrap_or_else(|| ("Unknown".to_string(), None));

        results.push(TrackedLinkWithClicks {
            id: link.id,
            tenant_id: link.tenant_id,
            tag_id: link.tag_id,
            slug: link.slug.clone(),
            target_url: link.target_url.clone(),
            created_at: link.created_at,
            click_count,
            tag_name: Some(tag_name),
            tag_color,
        });
    }

    Ok(Json(json!({"links": results})))
}

/// DELETE /api/tracked-links/:id — delete a tracked link
pub async fn delete_tracked_link(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let r = sqlx::query("DELETE FROM tracked_links WHERE id=$1 AND tenant_id=$2")
        .bind(id)
        .bind(tenant_id)
        .execute(&s.db)
        .await?;

    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Tracked link {id} not found")));
    }

    Ok(Json(json!({"message":"Deleted"})))
}

/// GET /track/:slug — public redirect endpoint (no auth)
/// Records the click if contact is identified via ?contact_id query param
pub async fn redirect_tracked_link(
    State(s): State<AppState>,
    Path(slug): Path<String>,
    Query(params): Query<serde_json::Value>,
) -> Result<impl IntoResponse, AppError> {
    let link = sqlx::query_as::<_, TrackedLink>(
        "SELECT * FROM tracked_links WHERE slug=$1"
    )
    .bind(&slug)
    .fetch_optional(&s.db)
    .await?
    .ok_or_else(|| AppError::NotFound(format!("Tracked link not found: {}", slug)))?;

    // Record the click if contact_id is provided
    if let Some(contact_id_str) = params.get("contact_id").and_then(|v| v.as_str()) {
        if let Ok(contact_id) = Uuid::parse_str(contact_id_str) {
            let _ = sqlx::query(
                "INSERT INTO link_clicks(id, tracked_link_id, contact_id, tenant_id)
                 VALUES($1, $2, $3, $4)"
            )
            .bind(Uuid::new_v4())
            .bind(link.id)
            .bind(contact_id)
            .bind(link.tenant_id)
            .execute(&s.db)
            .await;
        }
    }

    // Redirect to target URL
    let uri: Uri = link.target_url.parse().map_err(|_| AppError::BadRequest("Invalid target URL".into()))?;
    Ok(Redirect::to(uri.to_string().as_str()))
}

/// Generate a random alphanumeric slug
fn generate_slug(length: usize) -> String {
    let charset: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut rng = rand::thread_rng();
    (0..length)
        .map(|_| {
            let idx = rng.gen_range(0..charset.len());
            charset[idx] as char
        })
        .collect()
}
