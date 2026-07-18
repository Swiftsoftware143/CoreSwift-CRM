use axum::{extract::{State,Path,Json,Extension}, http::StatusCode, response::IntoResponse};
use serde_json::json;
use uuid::Uuid;
use crate::AppState;
use crate::errors::{AppError, ApiResult};
use crate::auth::Claims;
use super::models::*;

pub async fn list(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!({"lists": sqlx::query_as::<_,List>("SELECT * FROM lists WHERE tenant_id=$1 AND is_active=true ORDER BY name").bind(t).fetch_all(&s.db).await?})))
}
pub async fn create(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<CreateListRequest>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if r.name.is_empty() { return Err(AppError::Validation("Name required".into())); }
    let lt = r.list_type.unwrap_or_else(|| "static".into());
    if lt != "static" && lt != "dynamic" { return Err(AppError::Validation("list_type must be static/dynamic".into())); }
    let rules = r.rules.map(|r| serde_json::to_value(r).unwrap_or(serde_json::Value::Array(vec![])));
    Ok((StatusCode::CREATED, Json(json!(sqlx::query_as::<_,List>("INSERT INTO lists(id,tenant_id,name,description,list_type,rules) VALUES($1,$2,$3,$4,$5,$6) RETURNING *")
        .bind(Uuid::new_v4()).bind(t).bind(&r.name).bind(&r.description).bind(&lt).bind(&rules).fetch_one(&s.db).await.map_err(|e| {
            if let sqlx::Error::Database(ref d) = e { if d.constraint() == Some("lists_tenant_id_name_key") { return AppError::Duplicate(format!("List '{}' exists", r.name)); } }
            AppError::Database(e)
        })?))))
}
pub async fn get(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!(sqlx::query_as::<_,List>("SELECT * FROM lists WHERE id=$1 AND tenant_id=$2").bind(id).bind(t).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("List {id} not found")))?)))
}
pub async fn update(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>, Json(r): Json<UpdateListRequest>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let rules = r.rules.map(|r| serde_json::to_value(r).unwrap_or(serde_json::Value::Array(vec![])));
    Ok(Json(json!(sqlx::query_as::<_,List>("UPDATE lists SET name=COALESCE($1,name), description=COALESCE($2,description), rules=COALESCE($3,rules), is_active=COALESCE($4,is_active), updated_at=NOW() WHERE id=$5 AND tenant_id=$6 RETURNING *")
        .bind(&r.name).bind(&r.description).bind(&rules).bind(r.is_active).bind(id).bind(t).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("List {id} not found")))?)))
}
pub async fn delete(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM lists WHERE id=$1 AND tenant_id=$2").bind(id).bind(t).execute(&s.db).await?;
    if r.rows_affected() == 0 { return Err(AppError::NotFound(format!("List {id} not found"))); }
    Ok(Json(json!({"message":"Deleted"})))
}
pub async fn list_members(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!({"members": sqlx::query_as::<_,ListMember>("SELECT * FROM list_members WHERE list_id=$1 AND tenant_id=$2 ORDER BY created_at DESC").bind(id).bind(t).fetch_all(&s.db).await?})))
}
pub async fn add_member(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>, Json(r): Json<AddMemberRequest>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let m = sqlx::query_as::<_,ListMember>("INSERT INTO list_members(id,list_id,contact_id,tenant_id,added_manually) VALUES($1,$2,$3,$4,true) RETURNING *")
        .bind(Uuid::new_v4()).bind(id).bind(r.contact_id).bind(t).fetch_one(&s.db).await.map_err(|e| {
            if let sqlx::Error::Database(ref d) = e { if d.constraint() == Some("list_members_list_id_contact_id_key") { return AppError::Duplicate("Already a member".into()); } }
            AppError::Database(e)
        })?;
    crate::automation::engine::fire_list_trigger(&s.db, t, r.contact_id, id, "ListAdded").await;
    Ok((StatusCode::CREATED, Json(json!(m))))
}
pub async fn remove_member(State(s): State<AppState>, Extension(c): Extension<Claims>, Path((lid,cid)): Path<(Uuid,Uuid)>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM list_members WHERE list_id=$1 AND contact_id=$2 AND tenant_id=$3").bind(lid).bind(cid).bind(t).execute(&s.db).await?;
    if r.rows_affected() == 0 { return Err(AppError::NotFound("Member not found".into())); }
    crate::automation::engine::fire_list_trigger(&s.db, t, cid, lid, "ListRemoved").await;
    Ok(Json(json!({"message":"Removed"})))
}
pub async fn evaluate_list(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let list = sqlx::query_as::<_,List>("SELECT * FROM lists WHERE id=$1 AND tenant_id=$2").bind(id).bind(t).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("List {id} not found")))?;
    if list.list_type != "dynamic" { return Err(AppError::BadRequest("Only dynamic lists can be evaluated".into())); }
    Ok(Json(json!(crate::lists::evaluator::evaluate_dynamic_list(&s.db, &list).await?)))
}
