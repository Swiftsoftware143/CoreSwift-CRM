//! Pre-populate engine — scrapes public data sources to fill directory listings.
//!
//! When a business signs up, this attempts to pull information from public
//! sources (Google Places, Facebook, etc.) to pre-fill their listing.
//! Currently a stub with the structure for future integrations.

use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

/// Attempt to pre-populate directory listing data from public sources.
/// When a business signs up, this scrapes public info to fill their listing.
/// Returns the UUID of the prepopulated data record.
pub async fn prepopulate_listing(
    db: &PgPool,
    tenant_id: Uuid,
    business_name: &str,
    website: Option<&str>,
) -> Uuid {
    let data_id = Uuid::new_v4();

    let data = json!({
        "name": business_name,
        "website": website,
        "suggested_description": "",
        "suggested_hours": null,
        "suggested_keywords": [],
        "suggested_logo_url": null,
    });

    // For now this is a stub. In production, call:
    // - Google Places API for business data
    // - Facebook Graph API for page info
    // - Social media profile scraping
    let preview_link = format!("/preview/{}", data_id);

    let _ = sqlx::query(
        r#"INSERT INTO prepopulated_data (id, tenant_id, entity_type, data, preview_link)
           VALUES ($1, $2, 'directory_listing', $3, $4)
           ON CONFLICT (id) DO UPDATE SET data = $3, preview_link = $4, updated_at = NOW()"#,
    )
    .bind(data_id)
    .bind(tenant_id)
    .bind(&data)
    .bind(&preview_link)
    .execute(db)
    .await;

    data_id
}
