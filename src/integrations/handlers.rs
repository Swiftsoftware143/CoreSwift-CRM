use super::models::*;
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
        json!({"integrations": sqlx::query_as::<_,Integration>("SELECT * FROM integrations WHERE tenant_id=$1 ORDER BY name").bind(t).fetch_all(&s.db).await?}),
    ))
}
pub async fn create(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(r): Json<CreateIntegrationRequest>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if r.name.is_empty() || r.provider.is_empty() {
        return Err(AppError::Validation("Name and provider required".into()));
    }

    // Usage ceiling (`limit_integrations`): the ADD route only. `enabled = false` on the plan
    // resolves to ceiling 0, i.e. "connecting an integration is not available on your plan" — 402,
    // the same upsell as every other ceiling (kanban t_f49e4299).
    let integration_usage = crate::features::count_integrations(&s.db, t).await;
    crate::features::enforce_usage_limit(
        &s.db,
        t,
        "limit_integrations",
        "Integration",
        "integrations",
        integration_usage,
    )
    .await?;

    // `$4::integration_provider` cast a varchar column to a TYPE THAT DOES NOT EXIST (no migration
    // ever created it, and `pg_type` has no such row), so EVERY call to this route answered 500
    // "type integration_provider does not exist" — the integrations module could not add a row at
    // all, which is also why `limit_integrations` had nothing to count (kanban t_f49e4299). The
    // column is `varchar` and the Rust model is `String`, so the cast is simply removed.
    //
    // Second trap in the same statement: `config` is `jsonb NOT NULL DEFAULT '{}'`, and binding the
    // request's `Option<Value>` binds an explicit NULL — which OVERRIDES the column default and
    // raised `null value in column "config" … violates not-null constraint` (the 500 that survived
    // the cast fix, read from the container log during t_f49e4299's live proof). COALESCE lets an
    // omitted config take the column's own default, exactly as `update` already does.
    Ok((StatusCode::CREATED, Json(json!(sqlx::query_as::<_,Integration>("INSERT INTO integrations(id,tenant_id,name,provider,config) VALUES($1,$2,$3,$4,COALESCE($5,'{}'::jsonb)) RETURNING *")
        .bind(Uuid::new_v4()).bind(t).bind(&r.name).bind(&r.provider).bind(&r.config).fetch_one(&s.db).await?))))
}
pub async fn get(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!(sqlx::query_as::<_, Integration>(
        "SELECT * FROM integrations WHERE id=$1 AND tenant_id=$2"
    )
    .bind(id)
    .bind(t)
    .fetch_optional(&s.db)
    .await?
    .ok_or(AppError::NotFound(format!(
        "Integration {id} not found"
    )))?)))
}
pub async fn update(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(r): Json<UpdateIntegrationRequest>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(json!(sqlx::query_as::<_,Integration>("UPDATE integrations SET name=COALESCE($1,name), config=COALESCE($2,config), is_active=COALESCE($3,is_active), updated_at=NOW() WHERE id=$4 AND tenant_id=$5 RETURNING *")
        .bind(&r.name).bind(&r.config).bind(r.is_active).bind(id).bind(t).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("Integration {id} not found")))?)))
}
pub async fn delete(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM integrations WHERE id=$1 AND tenant_id=$2")
        .bind(id)
        .bind(t)
        .execute(&s.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Integration {id} not found")));
    }
    Ok(Json(json!({"message":"Deleted"})))
}
pub async fn list_mappings(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(iid): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    Ok(Json(
        json!({"mappings": sqlx::query_as::<_,TagMapping>("SELECT tm.* FROM tag_mappings tm JOIN integrations i ON i.id = tm.integration_id WHERE tm.integration_id=$1 AND i.tenant_id=$2").bind(iid).bind(t).fetch_all(&s.db).await?}),
    ))
}
pub async fn create_mapping(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(iid): Path<Uuid>,
    Json(r): Json<CreateMappingRequest>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let dir = r.direction.unwrap_or_else(|| "bidirectional".into());
    // The integration must belong to this tenant before tags can be mapped onto it.
    sqlx::query("SELECT 1 FROM integrations WHERE id=$1 AND tenant_id=$2")
        .bind(iid)
        .bind(t)
        .fetch_optional(&s.db)
        .await?
        .ok_or(AppError::NotFound(format!("Integration {iid} not found")))?;
    Ok((StatusCode::CREATED, Json(json!(sqlx::query_as::<_,TagMapping>("INSERT INTO tag_mappings(id,integration_id,tag_id,external_system,external_id,external_name,direction) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING *")
        .bind(Uuid::new_v4()).bind(iid).bind(r.tag_id).bind(&r.external_system).bind(&r.external_id).bind(&r.external_name).bind(&dir).fetch_one(&s.db).await?))))
}
pub async fn delete_mapping(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM tag_mappings WHERE id=$1 AND integration_id IN (SELECT id FROM integrations WHERE tenant_id=$2)")
        .bind(id)
        .bind(t)
        .execute(&s.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Mapping {id} not found")));
    }
    Ok(Json(json!({"message":"Deleted"})))
}
/// First/last three characters — the same shape the provider-key and portfolio panels use.
fn mask_secret(value: &str) -> String {
    if value.len() > 6 {
        format!("{}...{}", &value[..3], &value[value.len() - 3..])
    } else {
        "***".to_string()
    }
}

/// A webhook endpoint as the API may show it: `secret` is stored SEALED (t_6718dc86) and is
/// never echoed raw. The field keeps its name so existing callers keep parsing, but it now
/// carries the MASK; `has_secret` says whether anything is stored at all. The update path
/// accepts that mask back and keeps the stored secret instead of overwriting it with the mask.
fn webhook_json(w: &Webhook) -> serde_json::Value {
    let plain = w
        .secret
        .as_deref()
        .map(|stored| crate::secret_box::open(w.tenant_id, stored))
        .unwrap_or_default();
    json!({
        "id": w.id,
        "tenant_id": w.tenant_id,
        "name": w.name,
        "url": w.url,
        "secret": if plain.is_empty() { serde_json::Value::Null } else { json!(mask_secret(&plain)) },
        "has_secret": !plain.is_empty(),
        "events": w.events,
        "retry_count": w.retry_count,
        "timeout_ms": w.timeout_ms,
        "is_active": w.is_active,
        "last_triggered_at": w.last_triggered_at,
        // The panel shows how many deliveries have failed since the last success; the column is
        // nullable so an untouched endpoint reports a real 0 rather than nothing (t_c6049ec8).
        "failure_count": w.failure_count.unwrap_or(0),
        "created_at": w.created_at,
        "updated_at": w.updated_at,
    })
}

pub async fn list_webhooks(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let rows = sqlx::query_as::<_, Webhook>(
        "SELECT * FROM webhook_endpoints WHERE tenant_id=$1 ORDER BY name",
    )
    .bind(t)
    .fetch_all(&s.db)
    .await?;
    Ok(Json(json!({
        "webhooks": rows.iter().map(webhook_json).collect::<Vec<_>>()
    })))
}

pub async fn create_webhook(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(r): Json<CreateWebhookRequest>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if r.name.is_empty() || r.url.is_empty() {
        return Err(AppError::Validation("Name and url required".into()));
    }
    // Seal the signing secret at rest (t_6718dc86). An empty/absent secret stays NULL.
    let stored_secret = match r.secret.as_deref().filter(|v| !v.is_empty()) {
        Some(plain) => Some(crate::secret_box::seal(t, plain)?),
        None => None,
    };
    // `events` is NOT NULL DEFAULT '{}' in the schema, so binding the request's Option directly
    // stored SQL NULL and answered 500 on any create that omitted the array (t_6718dc86).
    let row = sqlx::query_as::<_, Webhook>("INSERT INTO webhook_endpoints(id,tenant_id,name,url,secret,events,retry_count,timeout_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING *")
        .bind(Uuid::new_v4()).bind(t).bind(&r.name).bind(&r.url).bind(&stored_secret).bind(r.events.unwrap_or_default()).bind(r.retry_count.unwrap_or(3)).bind(r.timeout_ms.unwrap_or(30_000)).fetch_one(&s.db).await?;
    Ok((StatusCode::CREATED, Json(webhook_json(&row))))
}

pub async fn update_webhook(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(r): Json<UpdateWebhookRequest>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    // A client that round-trips the value it was shown would otherwise overwrite the real secret
    // with the MASK (the same guard the provider-key upsert uses).
    let stored_secret = match r.secret.as_deref().filter(|v| !v.is_empty()) {
        None => None,
        Some(submitted) => {
            let existing: Option<String> = sqlx::query_scalar(
                "SELECT secret FROM webhook_endpoints WHERE id=$1 AND tenant_id=$2",
            )
            .bind(id)
            .bind(t)
            .fetch_optional(&s.db)
            .await?
            .flatten();
            let submitted_is_mask = existing
                .as_deref()
                .map(|cur| {
                    let cur_plain = crate::secret_box::open(t, cur);
                    !cur_plain.is_empty() && mask_secret(&cur_plain) == submitted
                })
                .unwrap_or(false);
            if submitted_is_mask {
                existing
            } else {
                Some(crate::secret_box::seal(t, submitted)?)
            }
        }
    };
    let row = sqlx::query_as::<_, Webhook>("UPDATE webhook_endpoints SET name=COALESCE($1,name), url=COALESCE($2,url), secret=COALESCE($3,secret), events=COALESCE($4,events), retry_count=COALESCE($5,retry_count), timeout_ms=COALESCE($6,timeout_ms), is_active=COALESCE($7,is_active), updated_at=NOW() WHERE id=$8 AND tenant_id=$9 RETURNING *")
        .bind(&r.name).bind(&r.url).bind(&stored_secret).bind(&r.events).bind(r.retry_count).bind(r.timeout_ms).bind(r.is_active).bind(id).bind(t).fetch_optional(&s.db).await?.ok_or(AppError::NotFound(format!("Webhook {id} not found")))?;
    Ok(Json(webhook_json(&row)))
}

pub async fn delete_webhook(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let t = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM webhook_endpoints WHERE id=$1 AND tenant_id=$2")
        .bind(id)
        .bind(t)
        .execute(&s.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(AppError::NotFound(format!("Webhook {id} not found")));
    }
    Ok(Json(json!({"message":"Deleted"})))
}
