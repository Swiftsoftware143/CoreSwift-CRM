use axum::{extract::{State,Extension,Json}, response::IntoResponse};
use serde_json::json;
use uuid::Uuid;
use crate::AppState;
use crate::errors::ApiResult;
use crate::auth::Claims;

pub async fn pipeline_stats(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| crate::errors::AppError::Unauthorized)?;
    let pipelines = sqlx::query_as::<_,(Uuid,String,i64)>("SELECT p.id, p.name, COUNT(o.id) FROM pipelines p LEFT JOIN opportunities o ON o.pipeline_id=p.id AND o.tenant_id=p.tenant_id WHERE p.tenant_id=$1 AND p.is_active=true GROUP BY p.id,p.name ORDER BY p.name")
        .bind(t).fetch_all(&s.db).await?;
    let mut data = Vec::new();
    for (id, name, total) in &pipelines {
        let won: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM opportunities WHERE pipeline_id=$1 AND status='won' AND tenant_id=$2").bind(id).bind(t).fetch_one(&s.db).await.unwrap_or(0);
        let lost: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM opportunities WHERE pipeline_id=$1 AND status='lost' AND tenant_id=$2").bind(id).bind(t).fetch_one(&s.db).await.unwrap_or(0);
        data.push(json!({"pipeline_id": id, "pipeline_name": name, "total": total, "won": won, "lost": lost, "open": total - won - lost}));
    }
    Ok(Json(json!({"pipelines": data})))
}

pub async fn score_distribution(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| crate::errors::AppError::Unauthorized)?;
    let d = sqlx::query_as::<_,(String,i64)>("SELECT category, COUNT(*) FROM scores WHERE tenant_id=$1 GROUP BY category ORDER BY category").bind(t).fetch_all(&s.db).await?;
    let tot: i64 = d.iter().map(|(_,c)| c).sum();
    Ok(Json(json!({"distribution": d, "total": tot})))
}

pub async fn tag_usage(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| crate::errors::AppError::Unauthorized)?;
    Ok(Json(json!({"tags": sqlx::query_as::<_,(Uuid,String,String,i64)>(
        "SELECT t.id,t.name,COALESCE(tc.name,'Uncategorized'),COUNT(ta.id) FROM tags t LEFT JOIN tag_categories tc ON tc.id=t.category_id LEFT JOIN tag_assignments ta ON ta.tag_id=t.id AND ta.tenant_id=t.tenant_id WHERE t.tenant_id=$1 AND t.is_active=true GROUP BY t.id,t.name,tc.name ORDER BY count DESC")
        .bind(t).fetch_all(&s.db).await?})))
}

pub async fn contact_stats(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| crate::errors::AppError::Unauthorized)?;
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM contacts WHERE tenant_id=$1 AND is_active=true").bind(t).fetch_one(&s.db).await.unwrap_or(0);
    let with_opps: i64 = sqlx::query_scalar("SELECT COUNT(DISTINCT contact_id) FROM opportunities WHERE tenant_id=$1 AND contact_id IS NOT NULL").bind(t).fetch_one(&s.db).await.unwrap_or(0);
    let with_tags: i64 = sqlx::query_scalar("SELECT COUNT(DISTINCT entity_id) FROM tag_assignments WHERE tenant_id=$1 AND entity_type='contact'").bind(t).fetch_one(&s.db).await.unwrap_or(0);
    let pct = if total > 0 { (with_tags as f64 / total as f64 * 100.0 * 100.0).round() / 100.0 } else { 0.0 };
    Ok(Json(json!({"total_contacts": total, "contacts_with_opportunities": with_opps, "contacts_with_tags": with_tags, "tagged_percentage": pct})))
}
