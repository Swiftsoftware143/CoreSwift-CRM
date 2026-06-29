use axum::{extract::{State, Path, Json, Extension, Query}, http::StatusCode, response::IntoResponse};
use rust_decimal::Decimal;
use serde_json::json;
use uuid::Uuid;
use crate::AppState;
use crate::errors::{AppError, ApiResult, validate_pagination};
use crate::auth::models::Claims;
use super::models::*;

fn count_or_zero(v: Option<i64>) -> i64 { v.unwrap_or(0) }

/// POST /api/affiliates/profile — Create affiliate profile
pub async fn create_profile(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<CreateAffiliateRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.tid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;

    // Check if already exists
    let existing = sqlx::query_scalar::<_, Option<i64>>("SELECT COUNT(*) FROM affiliates WHERE tenant_id = $1 AND user_id = $2")
        .bind(tid).bind(uid)
        .fetch_one(&s.db).await?;
    let existing = count_or_zero(existing);

    if existing > 0 {
        return Err(AppError::Duplicate("Affiliate profile already exists".to_string()));
    }

    // Get user name for code generation
    let user_name: Option<String> = sqlx::query_scalar::<_, Option<String>>("SELECT name FROM users WHERE id = $1")
        .bind(uid).fetch_optional(&s.db).await?.ok_or(AppError::Unauthorized)?;

    let code = generate_code(&user_name.unwrap_or_else(|| "affiliate".to_string()));
    let rate = r.commission_rate.unwrap_or(10.0);
    let ctype = r.commission_type.unwrap_or_else(|| "percentage".to_string());

    let aff = sqlx::query_as::<_, Affiliate>(
        r#"INSERT INTO affiliates (id, tenant_id, user_id, code, commission_rate, commission_type)
           VALUES ($1, $2, $3, $4, $5, $6) RETURNING *"#
    )
    .bind(Uuid::new_v4()).bind(tid).bind(uid).bind(&code)
    .bind(Decimal::try_from(rate).unwrap_or(Decimal::new(10, 1))).bind(&ctype)
    .fetch_one(&s.db).await?;

    Ok((StatusCode::CREATED, Json(json!(aff))))
}

/// GET /api/affiliates/profile — Get affiliate profile
pub async fn get_profile(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.tid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;

    let aff = sqlx::query_as::<_, Affiliate>("SELECT * FROM affiliates WHERE tenant_id = $1 AND user_id = $2")
        .bind(tid).bind(uid)
        .fetch_optional(&s.db).await?
        .ok_or(AppError::NotFound("No affiliate profile".to_string()))?;

    Ok(Json(json!(aff)))
}

/// PATCH /api/affiliates/profile — Update affiliate profile
pub async fn update_profile(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<UpdateAffiliateRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.tid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;

    let aff = sqlx::query_as::<_, Affiliate>(
        r#"UPDATE affiliates SET
            commission_rate = COALESCE($1, commission_rate),
            commission_type = COALESCE($2, commission_type),
            is_active = COALESCE($3, is_active),
            updated_at = NOW()
           WHERE tenant_id = $4 AND user_id = $5 RETURNING *"#
    )
    .bind(r.commission_rate.map(|v| Decimal::try_from(v).unwrap_or(Decimal::new(10, 1))))
    .bind(r.commission_type)
    .bind(r.is_active)
    .bind(tid).bind(uid)
    .fetch_one(&s.db).await?;

    Ok(Json(json!(aff)))
}

/// GET /api/affiliates/referrals — List referrals for this affiliate
pub async fn list_referrals(State(s): State<AppState>, Extension(c): Extension<Claims>, Query(p): Query<serde_json::Value>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.tid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;
    let (page, per_page) = validate_pagination(p.get("page").and_then(|v| v.as_i64()), p.get("per_page").and_then(|v| v.as_i64()));
    let offset = (page - 1) * per_page;

    // Get affiliate id
    let aff_id: Uuid = sqlx::query_scalar::<_, Uuid>("SELECT id FROM affiliates WHERE tenant_id = $1 AND user_id = $2")
        .bind(tid).bind(uid)
        .fetch_optional(&s.db).await?
        .ok_or(AppError::NotFound("No affiliate profile".to_string()))?;

    let referrals = sqlx::query_as::<_, Referral>(
        "SELECT * FROM referrals WHERE affiliate_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3"
    )
    .bind(aff_id).bind(per_page).bind(offset)
    .fetch_all(&s.db).await?;

    let total = sqlx::query_scalar::<_, Option<i64>>("SELECT COUNT(*) FROM referrals WHERE affiliate_id = $1")
        .bind(aff_id)
        .fetch_one(&s.db).await?;
    let total = count_or_zero(total);

    Ok(Json(json!({"referrals": referrals, "total": total, "page": page, "per_page": per_page})))
}

/// GET /api/affiliates/payouts — List commission payouts
pub async fn list_payouts(State(s): State<AppState>, Extension(c): Extension<Claims>, Query(p): Query<serde_json::Value>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.tid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;
    let (page, per_page) = validate_pagination(p.get("page").and_then(|v| v.as_i64()), p.get("per_page").and_then(|v| v.as_i64()));
    let offset = (page - 1) * per_page;

    let aff_id: Uuid = sqlx::query_scalar::<_, Uuid>("SELECT id FROM affiliates WHERE tenant_id = $1 AND user_id = $2")
        .bind(tid).bind(uid)
        .fetch_optional(&s.db).await?
        .ok_or(AppError::NotFound("No affiliate profile".to_string()))?;

    let payouts = sqlx::query_as::<_, CommissionPayout>(
        "SELECT * FROM commission_payouts WHERE affiliate_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3"
    )
    .bind(aff_id).bind(per_page).bind(offset)
    .fetch_all(&s.db).await?;

    let total = sqlx::query_scalar::<_, Option<i64>>("SELECT COUNT(*) FROM commission_payouts WHERE affiliate_id = $1")
        .bind(aff_id)
        .fetch_one(&s.db).await?;
    let total = count_or_zero(total);

    Ok(Json(json!({"payouts": payouts, "total": total, "page": page, "per_page": per_page})))
}

/// GET /api/affiliates/stats — Aggregated affiliate stats
pub async fn get_stats(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.tid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;

    let aff = sqlx::query_as::<_, Affiliate>("SELECT * FROM affiliates WHERE tenant_id = $1 AND user_id = $2")
        .bind(tid).bind(uid)
        .fetch_optional(&s.db).await?
        .ok_or(AppError::NotFound("No affiliate profile".to_string()))?;

    let total_refs = sqlx::query_scalar::<_, Option<i64>>("SELECT COUNT(*) FROM referrals WHERE affiliate_id = $1")
        .bind(aff.id).fetch_one(&s.db).await?;
    let total_refs = count_or_zero(total_refs);

    let pending_refs = sqlx::query_scalar::<_, Option<i64>>("SELECT COUNT(*) FROM referrals WHERE affiliate_id = $1 AND status = 'pending'")
        .bind(aff.id).fetch_one(&s.db).await?;
    let pending_refs = count_or_zero(pending_refs);

    let converted_refs = sqlx::query_scalar::<_, Option<i64>>("SELECT COUNT(*) FROM referrals WHERE affiliate_id = $1 AND status IN ('converted', 'commissioned', 'paid')")
        .bind(aff.id).fetch_one(&s.db).await?;
    let converted_refs = count_or_zero(converted_refs);

    let pending_amount: f64 = sqlx::query_scalar::<_, Option<rust_decimal::Decimal>>(
        "SELECT COALESCE(SUM(commission_amount), 0) FROM referrals WHERE affiliate_id = $1 AND status = 'commissioned'"
    ).bind(aff.id).fetch_one(&s.db).await
        .map(|v| v.map(|d| d.to_string().parse::<f64>().unwrap_or(0.0)).unwrap_or(0.0)).unwrap_or(0.0);

    let stats = AffiliateStats {
        total_referrals: total_refs,
        pending_referrals: pending_refs,
        converted_referrals: converted_refs,
        total_earned: aff.total_earned.to_string().parse::<f64>().unwrap_or(0.0),
        total_paid: aff.total_paid.to_string().parse::<f64>().unwrap_or(0.0),
        pending_payout: pending_amount,
    };

    Ok(Json(json!(stats)))
}

/// POST /api/affiliates/redeem/{code} — Apply an affiliate code to current tenant
pub async fn redeem_code(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(code): Path<String>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.tid).map_err(|_| AppError::Unauthorized)?;

    let aff = sqlx::query_as::<_, Affiliate>("SELECT * FROM affiliates WHERE code = $1 AND is_active = true")
        .bind(&code)
        .fetch_optional(&s.db).await?
        .ok_or(AppError::NotFound("Invalid or inactive affiliate code".to_string()))?;

    // Can't refer yourself
    if aff.tenant_id == tid {
        return Err(AppError::Validation("Cannot redeem your own affiliate code".to_string()));
    }

    // Check if already referred
    let existing = sqlx::query_scalar::<_, Option<i64>>("SELECT COUNT(*) FROM referrals WHERE affiliate_id = $1 AND referred_tenant_id = $2")
        .bind(aff.id).bind(tid)
        .fetch_one(&s.db).await?;
    let existing = count_or_zero(existing);

    if existing > 0 {
        return Err(AppError::Duplicate("Tenant already referred by this affiliate".to_string()));
    }

    let referral = sqlx::query_as::<_, Referral>(
        r#"INSERT INTO referrals (id, affiliate_id, referred_tenant_id, status)
           VALUES ($1, $2, $3, 'pending') RETURNING *"#
    )
    .bind(Uuid::new_v4()).bind(aff.id).bind(tid)
    .fetch_one(&s.db).await?;

    // Update affiliate referral count
    let _ = sqlx::query("UPDATE affiliates SET referral_count = referral_count + 1 WHERE id = $1")
        .bind(aff.id).execute(&s.db).await;

    Ok((StatusCode::CREATED, Json(json!(referral))))
}

// ── Affiliate Product Board ──

/// GET /api/affiliates/products — List affiliate products (public for FunnelSwift)
pub async fn list_products(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.tid).map_err(|_| AppError::Unauthorized)?;

    let products = sqlx::query_as::<_, AffiliateProduct>(
        "SELECT * FROM affiliate_products WHERE tenant_id = $1 ORDER BY sort_order ASC, name ASC"
    )
    .bind(tid)
    .fetch_all(&s.db)
    .await?;

    Ok(Json(json!({"products": products, "total": products.len()})))
}

/// POST /api/affiliates/products — Create a product
pub async fn create_product(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<CreateProductRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.tid).map_err(|_| AppError::Unauthorized)?;

    if r.name.is_empty() {
        return Err(AppError::Validation("Product name is required".into()));
    }

    let product = sqlx::query_as::<_, AffiliateProduct>(
        r#"INSERT INTO affiliate_products (id, tenant_id, name, description, price, commission_rate, commission_type, commission_amount, tag_id, image_url, checkout_url, sort_order)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING *"#
    )
    .bind(Uuid::new_v4())
    .bind(tid)
    .bind(&r.name)
    .bind(&r.description)
    .bind(Decimal::try_from(r.price).unwrap_or(Decimal::ZERO))
    .bind(Decimal::try_from(r.commission_rate.unwrap_or(10.0)).unwrap_or(Decimal::new(10, 1)))
    .bind(r.commission_type.as_deref().unwrap_or("percentage"))
    .bind(Decimal::try_from(r.commission_amount.unwrap_or(0.0)).unwrap_or(Decimal::ZERO))
    .bind(r.tag_id)
    .bind(&r.image_url)
    .bind(&r.checkout_url)
    .bind(r.sort_order.unwrap_or(0))
    .fetch_one(&s.db)
    .await?;

    Ok((StatusCode::CREATED, Json(json!(product))))
}

/// PATCH /api/affiliates/products/{id} — Update a product
pub async fn update_product(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>, Json(r): Json<UpdateProductRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.tid).map_err(|_| AppError::Unauthorized)?;

    let product = sqlx::query_as::<_, AffiliateProduct>(
        r#"UPDATE affiliate_products SET
            name = COALESCE($1, name),
            description = COALESCE($2, description),
            price = COALESCE($3, price),
            commission_rate = COALESCE($4, commission_rate),
            commission_type = COALESCE($5, commission_type),
            commission_amount = COALESCE($6, commission_amount),
            tag_id = COALESCE($7, tag_id),
            image_url = COALESCE($8, image_url),
            checkout_url = COALESCE($9, checkout_url),
            is_active = COALESCE($10, is_active),
            sort_order = COALESCE($11, sort_order),
            updated_at = NOW()
           WHERE id = $12 AND tenant_id = $13 RETURNING *"#
    )
    .bind(&r.name)
    .bind(&r.description)
    .bind(r.price.map(|v| Decimal::try_from(v).unwrap_or(Decimal::ZERO)))
    .bind(r.commission_rate.map(|v| Decimal::try_from(v).unwrap_or(Decimal::new(10, 1))))
    .bind(r.commission_type.as_deref())
    .bind(r.commission_amount.map(|v| Decimal::try_from(v).unwrap_or(Decimal::ZERO)))
    .bind(r.tag_id)
    .bind(&r.image_url)
    .bind(&r.checkout_url)
    .bind(r.is_active)
    .bind(r.sort_order)
    .bind(id)
    .bind(tid)
    .fetch_optional(&s.db)
    .await?
    .ok_or(AppError::NotFound("Product not found".into()))?;

    Ok(Json(json!(product)))
}

/// DELETE /api/affiliates/products/{id} — Delete a product
pub async fn delete_product(State(s): State<AppState>, Extension(c): Extension<Claims>, Path(id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.tid).map_err(|_| AppError::Unauthorized)?;
    let r = sqlx::query("DELETE FROM affiliate_products WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tid)
        .execute(&s.db)
        .await?;
    if r.rows_affected() == 0 { return Err(AppError::NotFound("Product not found".into())); }
    Ok(Json(json!({"message": "Product deleted"})))
}

/// GET /api/affiliates/products/tags — Get products grouped by tag (for FunnelSwift display)
pub async fn products_by_tag(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.tid).map_err(|_| AppError::Unauthorized)?;

    let products = sqlx::query_as::<_, (serde_json::Value,)>(
        r#"SELECT ap.*, t.name as tag_name, t.color as tag_color
           FROM affiliate_products ap
           LEFT JOIN tags t ON t.id = ap.tag_id
           WHERE ap.tenant_id = $1 AND ap.is_active = true
           ORDER BY ap.sort_order ASC, ap.name ASC"#
    )
    .bind(tid)
    .fetch_all(&s.db)
    .await?;

    Ok(Json(json!({"products": products})))
}

// ── Affiliate Self-Serve Product Selection ──
// Affiliates log into FunnelSwift back-end and pick which products to promote

/// GET /api/affiliates/my-products — Get products I'm promoting + products available
pub async fn list_my_products(State(s): State<AppState>, Extension(c): Extension<Claims>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.tid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;

    let aff_id: Uuid = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM affiliates WHERE tenant_id = $1 AND user_id = $2"
    )
    .bind(tid).bind(uid)
    .fetch_optional(&s.db).await?
    .ok_or(AppError::NotFound("Affiliate profile not found. Create one first.".into()))?;

    // Products I'm currently promoting
    let my_products = sqlx::query_as::<_, (serde_json::Value,)>(
        r#"SELECT ap.*, aps.is_active as promoting, aps.promo_link, aps.custom_commission_rate, aps.selected_at
           FROM affiliate_product_selections aps
           JOIN affiliate_products ap ON ap.id = aps.product_id
           WHERE aps.affiliate_id = $1 AND ap.is_active = true
           ORDER BY aps.selected_at DESC"#
    )
    .bind(aff_id)
    .fetch_all(&s.db)
    .await?;

    // Products available but not yet selected
    let available = sqlx::query_as::<_, (serde_json::Value,)>(
        r#"SELECT ap.*,
              CASE WHEN aps.id IS NOT NULL THEN true ELSE false END as already_selected
           FROM affiliate_products ap
           LEFT JOIN affiliate_product_selections aps ON aps.product_id = ap.id AND aps.affiliate_id = $1
           WHERE ap.tenant_id = $2 AND ap.is_active = true AND aps.id IS NULL
           ORDER BY ap.sort_order ASC, ap.name ASC"#
    )
    .bind(aff_id)
    .bind(tid)
    .fetch_all(&s.db)
    .await?;

    Ok(Json(json!({
        "promoting": my_products,
        "available": available,
        "total_promoting": my_products.len(),
        "total_available": available.len(),
    })))
}

/// POST /api/affiliates/my-products/select — Start promoting a product
pub async fn select_product(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<SelectProductRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.tid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;

    let aff_id: Uuid = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM affiliates WHERE tenant_id = $1 AND user_id = $2"
    )
    .bind(tid).bind(uid)
    .fetch_optional(&s.db).await?
    .ok_or(AppError::NotFound("Affiliate profile not found".into()))?;

    // Check product exists and belongs to this tenant
    let product = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM affiliate_products WHERE id = $1 AND tenant_id = $2 AND is_active = true"
    )
    .bind(r.product_id).bind(tid)
    .fetch_one(&s.db).await
        .unwrap_or(0);

    if product == 0 {
        return Err(AppError::NotFound("Product not found or not active".into()));
    }

    let selection = sqlx::query_as::<_, (serde_json::Value,)>(
        r#"INSERT INTO affiliate_product_selections (id, affiliate_id, product_id, is_active, promo_link, custom_commission_rate)
           VALUES ($1, $2, $3, true, $4, $5)
           ON CONFLICT (affiliate_id, product_id) DO UPDATE SET is_active = true, updated_at = NOW()
           RETURNING *"#
    )
    .bind(Uuid::new_v4())
    .bind(aff_id)
    .bind(r.product_id)
    .bind(&r.promo_link)
    .bind(r.custom_commission_rate)
    .fetch_one(&s.db)
    .await?;

    Ok(Json(json!({"message": "Product selected for promotion", "selection": selection.0})))
}

/// POST /api/affiliates/my-products/unselect — Stop promoting a product
pub async fn unselect_product(State(s): State<AppState>, Extension(c): Extension<Claims>, Json(r): Json<SelectProductRequest>) -> ApiResult<impl IntoResponse> {
    let tid = Uuid::parse_str(&c.tid).map_err(|_| AppError::Unauthorized)?;
    let uid = Uuid::parse_str(&c.sub).map_err(|_| AppError::Unauthorized)?;

    let aff_id: Uuid = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM affiliates WHERE tenant_id = $1 AND user_id = $2"
    )
    .bind(tid).bind(uid)
    .fetch_optional(&s.db).await?
    .ok_or(AppError::NotFound("Affiliate profile not found".into()))?;

    sqlx::query(
        "UPDATE affiliate_product_selections SET is_active = false, updated_at = NOW() WHERE affiliate_id = $1 AND product_id = $2"
    )
    .bind(aff_id)
    .bind(r.product_id)
    .execute(&s.db)
    .await?;

    Ok(Json(json!({"message": "Product unselected"})))
}
