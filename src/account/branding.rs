//! Email-branding HTTP surface (kanban t_feab8aff, porting t_c06a32eb).
//!
//! * `POST`/`DELETE /api/account/branding/logo` — authenticated, the CALLER'S OWN workspace. The id
//!   comes from the JWT claim (`aid`), never from the path, so there is nothing to spoof. These are
//!   the ONLY writers of `tenants.settings -> email_branding -> logo_url`.
//! * `GET /api/branding/logo/:tenant_id` — PUBLIC (see `crate::auth::route_policy`), because a MAIL
//!   CLIENT fetches the `<img src>` with no credential at all. Returns one thing: the image that
//!   workspace uploaded, keyed by an unguessable uuid; 404 when there is none.
//!
//! The accept-store-serve mechanics live in `crate::image_store` (built once, shared) and the domain
//! rules in `crate::branding`; this module only wires them to HTTP and to the caller's tenant.

use axum::{
    extract::{Extension, Multipart, Path, State},
    response::Response,
    Json,
};
use serde_json::json;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// The jsonb path this module writes, expressed once so the statements cannot drift.
const LOGO_PATH: &str = "{email_branding,logo_url}";

const UPSERT_LOGO: &str = "INSERT INTO tenant_logos (tenant_id, content_type, bytes, updated_at) \
     VALUES ($1, $2, $3, NOW()) \
     ON CONFLICT (tenant_id) DO UPDATE \
       SET content_type = EXCLUDED.content_type, \
           bytes        = EXCLUDED.bytes, \
           updated_at   = NOW()";

const SET_LOGO_URL: &str = "UPDATE tenants \
      SET settings = CASE \
            WHEN settings IS NULL OR jsonb_typeof(settings) = 'object' \
            THEN jsonb_set(COALESCE(settings, '{}'::jsonb), $1::text[], to_jsonb($2::text), true) \
            ELSE settings \
          END, \
          updated_at = NOW() \
    WHERE id = $3";

const CLEAR_LOGO_URL: &str = "UPDATE tenants \
      SET settings = CASE \
            WHEN settings IS NULL OR jsonb_typeof(settings) = 'object' \
            THEN jsonb_set(COALESCE(settings, '{}'::jsonb), $1::text[], to_jsonb(''::text), true) \
            ELSE settings \
          END, \
          updated_at = NOW() \
    WHERE id = $2";

/// `POST /api/account/branding/logo` — accept an image, store it, and point the tenant's branding
/// document at it.
///
/// The URL is version-stamped (`?v=<epoch>`) so a mail client and a browser key on the bytes rather
/// than on a cached 200. `brand_name` / `brand_color` are NOT touched: one writer per field.
pub async fn upload_logo(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    mut multipart: Multipart,
) -> ApiResult<Json<serde_json::Value>> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let (content_type, bytes) = crate::image_store::read_uploaded_image(&mut multipart).await?;
    let byte_len = bytes.len();

    sqlx::query(UPSERT_LOGO)
        .bind(tenant_id)
        .bind(&content_type)
        .bind(&bytes)
        .execute(&s.db)
        .await?;

    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let logo_url = format!("/api/branding/logo/{tenant_id}?v={epoch}");

    // Only merge when the document really is an object; a row holding an array must not be replaced
    // with a branding object (the console's own settings write owns everything else in it).
    sqlx::query(SET_LOGO_URL)
        .bind(LOGO_PATH)
        .bind(&logo_url)
        .bind(tenant_id)
        .execute(&s.db)
        .await?;

    Ok(Json(json!({
        "logo_url": logo_url,
        "content_type": content_type,
        "bytes": byte_len,
    })))
}

/// `DELETE /api/account/branding/logo` — forget the bytes and clear ONLY `logo_url`.
///
/// The brand name and colour survive, so a tenant can swap a logo without re-typing their identity.
pub async fn delete_logo(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<Json<serde_json::Value>> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;

    let removed = sqlx::query("DELETE FROM tenant_logos WHERE tenant_id = $1")
        .bind(tenant_id)
        .execute(&s.db)
        .await?
        .rows_affected();

    sqlx::query(CLEAR_LOGO_URL)
        .bind(LOGO_PATH)
        .bind(tenant_id)
        .execute(&s.db)
        .await?;

    Ok(Json(json!({"removed": removed > 0})))
}

/// `GET /api/branding/logo/:tenant_id` — PUBLIC: serve the stored bytes, or 404.
///
/// Deliberately reads nothing else: no name, no colour, no existence of the tenant beyond whether a
/// logo row exists. The tenant id is an unguessable uuid.
pub async fn serve_logo(
    State(s): State<AppState>,
    Path(tenant_id): Path<Uuid>,
) -> ApiResult<Response> {
    let row: Option<(String, Vec<u8>)> =
        sqlx::query_as("SELECT content_type, bytes FROM tenant_logos WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_optional(&s.db)
            .await?;
    match row {
        Some((content_type, bytes)) => crate::image_store::image_response(content_type, bytes),
        None => Err(AppError::NotFound("No logo for this account".into())),
    }
}
