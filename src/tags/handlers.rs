use axum::{extract::{State, Path, Json, Extension}, http::StatusCode, response::IntoResponse};
use serde_json::json;
use uuid::Uuid;
use crate::AppState;
use crate::errors::{AppError, ApiResult};
use crate::auth::models::Claims;
use crate::audit;
use super::models::*;

pub async fn list_categories(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!({"categories": sqlx::query_as::<_,TagCategory>("SELECT * FROM tag_categories WHERE tenant_id=$1 ORDER BY name").bind(t).fetch_all(&s.db).await?})))
}
pub async fn create_category(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<CreateCategoryRequest>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if r.name.is_empty() { return Err(AppError::Validation("Name required".into())); }
    let cat = sqlx::query_as::<_,TagCategory>("INSERT INTO tag_categories(id,tenant_id,name,color) VALUES($1,$2,$3,$4) RETURNING *")
        .bind(Uuid::new_v4()).bind(t).bind(&r.name).bind(&r.color).fetch_one(&s.db).await.map_err(|e| {
            if let sqlx::Error::Database(ref d) = e { if d.constraint() == Some("tag_categories_tenant_id_name_key") { return AppError::Duplicate(format!("Category '{}' exists", r.name)); } }
            AppError::Database(e)
        })?;
    Ok((StatusCode::CREATED, Json(json!(cat))))
}
pub async fn update_category(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>, Json(r): Json<UpdateCategoryRequest>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!(sqlx::query_as::<_,TagCategory>("UPDATE tag_categories SET name=COALESCE($1,name), color=COALESCE($2,color), updated_at=NOW() WHERE id=$3 AND tenant_id=$4 RETURNING *")
        .bind(&r.name).bind(&r.color).bind(id).bind(t).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("Category {id} not found")))?)))
}
pub async fn delete_category(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM tag_categories WHERE id=$1 AND tenant_id=$2").bind(id).bind(t).execute(&s.db).await?;
    if r.rows_affected() == 0 { return Err(AppError::NotFound(format!("Category {id} not found"))); }
    Ok(Json(json!({"message":"Deleted"})))
}

pub async fn list_tags(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!({"tags": sqlx::query_as::<_,Tag>("SELECT * FROM tags WHERE tenant_id=$1 AND is_active=true ORDER BY name").bind(t).fetch_all(&s.db).await?})))
}
pub async fn create_tag(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<CreateTagRequest>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if r.name.is_empty() { return Err(AppError::Validation("Name required".into())); }
    let tag = sqlx::query_as::<_,Tag>("INSERT INTO tags(id,tenant_id,category_id,name,color,parent_id) VALUES($1,$2,$3,$4,$5,$6) RETURNING *")
        .bind(Uuid::new_v4()).bind(t).bind(r.category_id).bind(&r.name).bind(&r.color).bind(r.parent_id).fetch_one(&s.db).await.map_err(|e| {
            if let sqlx::Error::Database(ref d) = e { if d.constraint() == Some("tags_tenant_id_name_key") { return AppError::Duplicate(format!("Tag '{}' exists", r.name)); } }
            AppError::Database(e)
        })?;
    Ok((StatusCode::CREATED, Json(json!(tag))))
}
pub async fn get_tag(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!(sqlx::query_as::<_,Tag>("SELECT * FROM tags WHERE id=$1 AND tenant_id=$2").bind(id).bind(t).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("Tag {id} not found")))?)))
}
pub async fn update_tag(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>, Json(r): Json<UpdateTagRequest>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;
    let tag = sqlx::query_as::<_,Tag>("UPDATE tags SET name=COALESCE($1,name), category_id=COALESCE($2,category_id), color=COALESCE($3,color), parent_id=COALESCE($4,parent_id), is_active=COALESCE($5,is_active), updated_at=NOW() WHERE id=$6 AND tenant_id=$7 RETURNING *")
        .bind(&r.name).bind(r.category_id).bind(&r.color).bind(r.parent_id).bind(r.is_active).bind(id).bind(t).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("Tag {id} not found")))?;
    audit::logger::log_event(&s.db, t, Some(uid), "tag.updated", "tag", Some(id), Some(json!({"updated": true})), None).await;
    Ok(Json(json!(tag)))
}
pub async fn delete_tag(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM tags WHERE id=$1 AND tenant_id=$2").bind(id).bind(t).execute(&s.db).await?;
    if r.rows_affected() == 0 { return Err(AppError::NotFound(format!("Tag {id} not found"))); }
    Ok(Json(json!({"message":"Deleted"})))
}

pub async fn assign_tag(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<AssignTagRequest>) -> ApiResult<impl IntoResponse> {
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let valid = ["contact","company","opportunity"];
    if !valid.contains(&r.entity_type.as_str()) { return Err(AppError::Validation("Invalid entity_type".into())); }
    sqlx::query_as::<_,Tag>("SELECT * FROM tags WHERE id=$1 AND tenant_id=$2").bind(r.tag_id).bind(t).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("Tag {} not found", r.tag_id)))?;
    let a = sqlx::query_as::<_,TagAssignment>("INSERT INTO tag_assignments(id,tag_id,entity_type,entity_id,tenant_id,assigned_by) VALUES($1,$2,$3,$4,$5,$6) RETURNING *")
        .bind(Uuid::new_v4()).bind(r.tag_id).bind(&r.entity_type).bind(r.entity_id).bind(t).bind(uid).fetch_one(&s.db).await.map_err(|e| {
            if let sqlx::Error::Database(ref d) = e { if d.constraint() == Some("tag_assignments_tag_id_entity_type_entity_id_key") { return AppError::Duplicate("Already assigned".into()); } }
            AppError::Database(e)
        })?;
    crate::automation::engine::fire_tag_trigger(&s.db,t,&r.entity_type,r.entity_id,r.tag_id,"TagAdded").await;
    Ok((StatusCode::CREATED, Json(json!(a))))
}
pub async fn unassign_tag(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let a = sqlx::query_as::<_,TagAssignment>("SELECT * FROM tag_assignments WHERE id=$1 AND tenant_id=$2").bind(id).bind(t).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("Assignment {id} not found")))?;
    sqlx::query("DELETE FROM tag_assignments WHERE id=$1 AND tenant_id=$2").bind(id).bind(t).execute(&s.db).await?;
    crate::automation::engine::fire_tag_trigger(&s.db,t,&a.entity_type,a.entity_id,a.tag_id,"TagRemoved").await;
    Ok(Json(json!({"message":"Unassigned"})))
}
pub async fn get_entity_tags(State(s): State<AppState>, Extension(c): Extension<Claims>, Path((et,eid)): Path<(String,Uuid)>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!({"tags": sqlx::query_as::<_,Tag>("SELECT t.* FROM tags t JOIN tag_assignments ta ON t.id=ta.tag_id WHERE ta.entity_type=$1 AND ta.entity_id=$2 AND ta.tenant_id=$3 AND t.is_active=true")
        .bind(&et).bind(eid).bind(t).fetch_all(&s.db).await?})))
}
