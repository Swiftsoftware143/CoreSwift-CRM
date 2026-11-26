use super::models::*;
use crate::audit;
use crate::auth::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;
use axum::{
    extract::{Extension, Json, Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde_json::json;
use uuid::Uuid;

pub async fn list(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(
        json!({"lists": sqlx::query_as::<_,List>("SELECT * FROM lists WHERE tenant_id=$1 AND is_active=true ORDER BY name").bind(t).fetch_all(&s.db).await?}),
    ))
}
/// Refuse a rule the engine cannot run.
///
/// Measured live (2026-11-25): a rule built on `city` saved 200 and then evaluated to zero members, so an
/// unsupported condition looked exactly like a list nobody qualifies for. The console's dropdowns offer
/// only these fields, but an API caller or an agent can send anything, and a saved rule that silently does
/// nothing is worse than an error because it is believed.
fn check_rules(rules: &[crate::lists::models::ListRule]) -> Result<(), AppError> {
    const FIELDS: [&str; 6] = [
        "tag",
        "score",
        "contact_field",
        "deal_source",
        "deal_stage",
        "deal_date",
    ];
    // Which operators each field can actually run. A pair the engine does not implement would answer with an
    // empty set — indistinguishable from "nobody qualifies" — so it is refused here instead.
    fn ops_for(field: &str) -> &'static [&'static str] {
        match field {
            "score" => &["gte", "lte", "equals"],
            "contact_field" => &["equals", "contains"],
            "deal_stage" => &["equals", "in"],
            _ => &["equals"],
        }
    }
    for r in rules {
        if !FIELDS.contains(&r.field.as_str()) {
            return Err(AppError::Validation(format!(
                "a rule cannot be built on \"{}\"; this list can evaluate: {}",
                r.field,
                FIELDS.join(", ")
            )));
        }
        let allowed = ops_for(&r.field);
        if !allowed.contains(&r.operator.as_str()) {
            return Err(AppError::Validation(format!(
                "\"{}\" cannot be used on \"{}\"; that field takes: {}",
                r.operator,
                r.field,
                allowed.join(", ")
            )));
        }
        if r.field == "contact_field" {
            let column = r.value.get("column").and_then(|v| v.as_str()).unwrap_or("");
            const COLUMNS: [&str; 9] = [
                "city",
                "state",
                "country",
                "job_title",
                "title",
                "company",
                "source",
                "score_category",
                "gender",
            ];
            if !COLUMNS.contains(&column) {
                return Err(AppError::Validation(format!(
                    "\"{column}\" is not a contact field a list can be built on; use one of: {}",
                    COLUMNS.join(", ")
                )));
            }
            if r.value
                .get("equals")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .is_empty()
            {
                return Err(AppError::Validation(format!(
                    "the rule on \"{column}\" has no value, so it can never match"
                )));
            }
        }
        let empty = match &r.value {
            serde_json::Value::Null => true,
            serde_json::Value::String(v) => v.trim().is_empty(),
            serde_json::Value::Array(v) => v.is_empty(),
            _ => false,
        };
        if empty {
            return Err(AppError::Validation(format!(
                "the rule on \"{}\" has no value, so it can never match",
                r.field
            )));
        }
    }
    Ok(())
}

pub async fn create(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(mut r): Json<CreateListRequest>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    // FREE-TEXT HYGIENE BEFORE THE WRITE (`security::free_text`). Measured live 2026-11-26:
    // `POST /api/lists {"name":"L\u0000ist"}` answered 500 "Database error"; the description had
    // the same hole.
    r.name = crate::security::free_text::single_line(&r.name, "Name", 255)?;
    r.description = crate::security::free_text::nul_free(r.description);
    let lt = r.list_type.unwrap_or_else(|| "static".into());
    if lt != "static" && lt != "dynamic" {
        return Err(AppError::Validation(
            "list_type must be static/dynamic".into(),
        ));
    }
    if let Some(ref rules) = r.rules {
        check_rules(rules)?;
    }
    let rules = r
        .rules
        .map(|r| serde_json::to_value(r).unwrap_or(serde_json::Value::Array(vec![])));
    Ok((StatusCode::CREATED, Json(json!(sqlx::query_as::<_,List>("INSERT INTO lists(id,tenant_id,name,description,list_type,dynamic_rules) VALUES($1,$2,$3,$4,$5,$6) RETURNING *")
        .bind(Uuid::new_v4()).bind(t).bind(&r.name).bind(&r.description).bind(&lt).bind(&rules).fetch_one(&s.db).await.map_err(|e| {
            if let sqlx::Error::Database(ref d) = e { if d.constraint() == Some("idx_lists_name_tenant") { return AppError::Duplicate(format!("List '{}' exists", r.name)); } }
            AppError::Database(e)
        })?))))
}
pub async fn get(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!(sqlx::query_as::<_, List>(
        "SELECT * FROM lists WHERE id=$1 AND tenant_id=$2"
    )
    .bind(id)
    .bind(t)
    .fetch_optional(&s.db)
    .await?
    .ok_or(AppError::NotFound(format!("List {id} not found")))?)))
}
pub async fn update(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(mut r): Json<UpdateListRequest>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    // Same hygiene as the create door (see `security::free_text`): an edit carrying a NUL put a
    // 500 on the update with nothing written.
    if let Some(raw) = r.name.as_deref() {
        r.name = Some(crate::security::free_text::single_line(raw, "Name", 255)?);
    }
    r.description = crate::security::free_text::nul_free(r.description);
    if let Some(ref rules) = r.rules {
        check_rules(rules)?;
    }
    // THE TYPE IS NOW WRITTEN (2026-11-25). `UpdateListRequest` has carried `list_type` since the field was
    // added, but the UPDATE bound only name/description/rules/is_active — so switching a list from static to
    // dynamic, or freezing a dynamic one as static, answered 200 and changed nothing. Whatever the request
    // accepts, this statement saves.
    if let Some(ref lt) = r.list_type {
        if lt != "static" && lt != "dynamic" {
            return Err(AppError::Validation(
                "list_type must be static or dynamic".into(),
            ));
        }
    }
    if r.list_type.as_deref() == Some("dynamic") && r.rules.is_none() {
        // A dynamic list with no rules has no members; say so rather than letting it look broken.
        let existing: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT dynamic_rules FROM lists WHERE id = $1 AND tenant_id = $2")
                .bind(id)
                .bind(t)
                .fetch_optional(&s.db)
                .await?;
        let empty = existing
            .as_ref()
            .map(|v| v.is_null() || v == &serde_json::json!([]))
            .unwrap_or(true);
        if empty {
            return Err(AppError::Validation(
                "a dynamic list needs at least one rule, or nobody is ever on it".into(),
            ));
        }
    }
    let rules = r
        .rules
        .map(|r| serde_json::to_value(r).unwrap_or(serde_json::Value::Array(vec![])));
    Ok(Json(json!(sqlx::query_as::<_,List>("UPDATE lists SET name=COALESCE($1,name), description=COALESCE($2,description), list_type=COALESCE($7,list_type), dynamic_rules=COALESCE($3,dynamic_rules), is_active=COALESCE($4,is_active), updated_at=NOW() WHERE id=$5 AND tenant_id=$6 RETURNING *")
        .bind(&r.name).bind(&r.description).bind(&rules).bind(r.is_active).bind(id).bind(t).bind(&r.list_type).fetch_optional(&s.db).await.map_err(|e| {
            // 086 added idx_lists_name_tenant, so renaming a list onto an existing name in the
            // same tenant is now a real violation on this path too.
            if let sqlx::Error::Database(ref d) = e {
                if d.constraint() == Some("idx_lists_name_tenant") {
                    return AppError::Duplicate(format!("List '{}' exists", r.name.clone().unwrap_or_default()));
                }
            }
            AppError::Database(e)
        })?.ok_or(AppError::NotFound(format!("List {id} not found")))?)))
}
pub async fn delete(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;
    // The name, read before the row goes (see contacts::delete).
    let gone: Option<String> =
        sqlx::query_scalar("SELECT name FROM lists WHERE id=$1 AND tenant_id=$2")
            .bind(id)
            .bind(t)
            .fetch_optional(&s.db)
            .await?;
    let r = sqlx::query("DELETE FROM lists WHERE id=$1 AND tenant_id=$2")
        .bind(id)
        .bind(t)
        .execute(&s.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("List {id} not found")));
    }
    if let Some(name) = gone {
        audit::logger::log_event(
            &s.db,
            t,
            Some(uid),
            "list.deleted",
            "list",
            Some(id),
            Some(json!({"name": name})),
            None,
        )
        .await;
    }
    Ok(Json(json!({"message":"Deleted"})))
}
pub async fn list_members(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(
        json!({"members": sqlx::query_as::<_,ListMember>("SELECT * FROM list_members WHERE list_id=$1 AND tenant_id=$2 ORDER BY added_at DESC").bind(id).bind(t).fetch_all(&s.db).await?}),
    ))
}
pub async fn add_member(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(r): Json<AddMemberRequest>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let m = sqlx::query_as::<_,ListMember>("INSERT INTO list_members(id,list_id,contact_id,tenant_id,added_manually) VALUES($1,$2,$3,$4,true) RETURNING *")
        .bind(Uuid::new_v4()).bind(id).bind(r.contact_id).bind(t).fetch_one(&s.db).await.map_err(|e| {
            if let sqlx::Error::Database(ref d) = e { if d.constraint() == Some("list_members_list_id_contact_id_key") { return AppError::Duplicate("Already a member".into()); } }
            AppError::Database(e)
        })?;
    crate::automation::engine::fire_list_trigger(&s.db, t, r.contact_id, id, "ListAdded").await;
    Ok((StatusCode::CREATED, Json(json!(m))))
}
pub async fn remove_member(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path((lid, cid)): Path<(Uuid, Uuid)>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let r =
        sqlx::query("DELETE FROM list_members WHERE list_id=$1 AND contact_id=$2 AND tenant_id=$3")
            .bind(lid)
            .bind(cid)
            .bind(t)
            .execute(&s.db)
            .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound("Member not found".into()));
    }
    crate::automation::engine::fire_list_trigger(&s.db, t, cid, lid, "ListRemoved").await;
    Ok(Json(json!({"message":"Removed"})))
}
pub async fn evaluate_list(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let list = sqlx::query_as::<_, List>("SELECT * FROM lists WHERE id=$1 AND tenant_id=$2")
        .bind(id)
        .bind(t)
        .fetch_optional(&s.db)
        .await?
        .ok_or(AppError::NotFound(format!("List {id} not found")))?;
    if list.list_type != "dynamic" {
        return Err(AppError::BadRequest(
            "Only dynamic lists can be evaluated".into(),
        ));
    }
    Ok(Json(json!(
        crate::lists::evaluator::evaluate_dynamic_list(&s.db, &list).await?
    )))
}
