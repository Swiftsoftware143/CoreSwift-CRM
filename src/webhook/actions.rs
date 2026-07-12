//! Webhook action router — maps action strings to actual database queries
//!
//! Each action corresponds to one or more CRM Swift features.
//! This is the glue that lets OpenClaw, n8n, and CheatLayer call any
//! endpoint through a single webhook.

use sqlx::PgPool;
use uuid::Uuid;
use serde_json::json;
use rand::Rng;

/// Route a webhook action to the correct handler.
/// Returns (status_code, response_body_json).
pub async fn route_action(
    db: &PgPool,
    tenant_id: Uuid,
    action: &str,
    params: Option<&serde_json::Value>,
    data: Option<&serde_json::Value>,
) -> Result<(i32, serde_json::Value), String> {
    let start = std::time::Instant::now();

    let result = match action {
        // ── Contacts ──
        "contacts.list" => {
            let limit = params.and_then(|p| p.get("limit").and_then(|v| v.as_i64())).unwrap_or(50);
            let offset = params.and_then(|p| p.get("offset").and_then(|v| v.as_i64())).unwrap_or(0);
            let contacts = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT id, first_name, last_name, email, phone, company_id, score, created_at FROM contacts WHERE tenant_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3"
            )
            .bind(tenant_id).bind(limit as i32).bind(offset as i32)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"contacts": contacts, "total": contacts.len()})))
        }
        "contacts.create" => {
            let body = data.ok_or("data required")?;
            let id = Uuid::new_v4();
            let first = body.get("first_name").and_then(|v| v.as_str()).unwrap_or("");
            let last = body.get("last_name").and_then(|v| v.as_str()).unwrap_or("");
            let email = body.get("email").and_then(|v| v.as_str()).unwrap_or("");
            let phone = body.get("phone").and_then(|v| v.as_str());
            sqlx::query(
                "INSERT INTO contacts (id, tenant_id, first_name, last_name, email, phone) VALUES ($1, $2, $3, $4, $5, $6)"
            )
            .bind(id).bind(tenant_id).bind(first).bind(last).bind(email).bind(phone)
            .execute(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((201, json!({"id": id, "created": true})))
        }
        "contacts.get" => {
            let id = params.and_then(|p| p.get("id").and_then(|v| v.as_str()))
                .ok_or("contact id required")?;
            let uid = Uuid::parse_str(id).map_err(|_| "invalid uuid".to_string())?;
            let contact = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT * FROM contacts WHERE id = $1 AND tenant_id = $2"
            )
            .bind(uid).bind(tenant_id)
            .fetch_optional(db).await
            .map_err(|e| format!("DB error: {}", e))?
            .ok_or("contact not found".to_string())?;
            Ok((200, contact.0))
        }

        // ── Tags ──
        "tags.list" => {
            let tags = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT id, name, color, category_id FROM tags WHERE tenant_id = $1 ORDER BY name"
            )
            .bind(tenant_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"tags": tags})))
        }
        "tags.assign" => {
            let body = data.ok_or("data required")?;
            let contact_id = body.get("contact_id").and_then(|v| v.as_str())
                .ok_or("contact_id required")?;
            let tag_id = body.get("tag_id").and_then(|v| v.as_str())
                .ok_or("tag_id required")?;
            let cid = Uuid::parse_str(contact_id).map_err(|_| "invalid contact_id".to_string())?;
            let tid = Uuid::parse_str(tag_id).map_err(|_| "invalid tag_id".to_string())?;
            sqlx::query(
                "INSERT INTO tag_assignments (id, tenant_id, entity_type, entity_id, tag_id) VALUES ($1, $2, 'contact', $3, $4) ON CONFLICT DO NOTHING"
            )
            .bind(Uuid::new_v4()).bind(tenant_id).bind(cid).bind(tid)
            .execute(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"assigned": true})))
        }

        // ── Lists ──
        "lists.list" => {
            let lists = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT id, name, list_type, created_at FROM lists WHERE tenant_id = $1 ORDER BY name"
            )
            .bind(tenant_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"lists": lists})))
        }
        "lists.members" => {
            let list_id = params.and_then(|p| p.get("id").and_then(|v| v.as_str()))
                .ok_or("list id required")?;
            let lid = Uuid::parse_str(list_id).map_err(|_| "invalid uuid".to_string())?;
            let members = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT c.id, c.first_name, c.last_name, c.email FROM list_members lm JOIN contacts c ON c.id = lm.contact_id WHERE lm.list_id = $1 AND lm.tenant_id = $2"
            )
            .bind(lid).bind(tenant_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"members": members})))
        }

        // ── Pipelines & Opportunities ──
        "pipelines.list" => {
            let pipelines = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT p.id, p.name, COALESCE((SELECT json_agg(json_build_object('id', ps.id, 'name', ps.name, 'color', ps.color, 'position', ps.position, 'probability', ps.probability)) FROM pipeline_stages ps WHERE ps.pipeline_id = p.id ORDER BY ps.position), '[]'::json) AS stages FROM pipelines p WHERE p.tenant_id = $1 ORDER BY p.name"
            )
            .bind(tenant_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"pipelines": pipelines})))
        }
        "pipelines.opportunities" => {
            let pipeline_id = params.and_then(|p| p.get("pipeline_id").and_then(|v| v.as_str()))
                .ok_or("pipeline_id required")?;
            let pid = Uuid::parse_str(pipeline_id).map_err(|_| "invalid uuid".to_string())?;
            let opportunities = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT o.id, o.name, o.value, o.stage_id, ps.name as stage_name, o.contact_id, o.created_at FROM opportunities o JOIN pipeline_stages ps ON ps.id = o.stage_id WHERE o.pipeline_id = $1 AND o.tenant_id = $2 ORDER BY o.created_at DESC"
            )
            .bind(pid).bind(tenant_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"opportunities": opportunities})))
        }

        // ── Affiliates ──
        "affiliates.profile" => {
            let profile = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT id, code, commission_rate, commission_type, total_earned, total_paid, referral_count, is_active FROM affiliates WHERE tenant_id = $1"
            )
            .bind(tenant_id)
            .fetch_optional(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"profile": profile})))
        }
        "affiliates.referrals" => {
            let referrals = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT r.*, a.code as affiliate_code FROM referrals r JOIN affiliates a ON a.id = r.affiliate_id WHERE a.tenant_id = $1 ORDER BY r.created_at DESC LIMIT 50"
            )
            .bind(tenant_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"referrals": referrals})))
        }
        "affiliates.stats" => {
            let stats = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT id, total_earned, total_paid, referral_count FROM affiliates WHERE tenant_id = $1"
            )
            .bind(tenant_id)
            .fetch_optional(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"stats": stats})))
        }

        // ── Affiliate Products ──
        "affiliate_products.list" => {
            let products = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT ap.*, t.name as tag_name FROM affiliate_products ap LEFT JOIN tags t ON t.id = ap.tag_id WHERE ap.tenant_id = $1 AND ap.is_active = true ORDER BY ap.sort_order ASC"
            )
            .bind(tenant_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"products": products})))
        }
        "affiliate_products.my" => {
            let affiliate_id = params.and_then(|p| p.get("affiliate_id").and_then(|v| v.as_str()))
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or("affiliate_id required")?;
            let products = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT ap.*, aps.is_active as promoting, aps.promo_link
                 FROM affiliate_product_selections aps
                 JOIN affiliate_products ap ON ap.id = aps.product_id
                 WHERE aps.affiliate_id = $1 AND ap.is_active = true"
            )
            .bind(affiliate_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"products": products})))
        }
        "affiliate_products.select" => {
            let body = data.ok_or("data required")?;
            let aff_id = body.get("affiliate_id").and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok()).ok_or("affiliate_id required")?;
            let prod_id = body.get("product_id").and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok()).ok_or("product_id required")?;
            sqlx::query(
                "INSERT INTO affiliate_product_selections (id, affiliate_id, product_id, is_active) VALUES ($1, $2, $3, true) ON CONFLICT (affiliate_id, product_id) DO UPDATE SET is_active = true, updated_at = NOW()"
            )
            .bind(Uuid::new_v4()).bind(aff_id).bind(prod_id)
            .execute(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"selected": true})))
        }
        "affiliate_products.unselect" => {
            let body = data.ok_or("data required")?;
            let aff_id = body.get("affiliate_id").and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok()).ok_or("affiliate_id required")?;
            let prod_id = body.get("product_id").and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok()).ok_or("product_id required")?;
            sqlx::query(
                "UPDATE affiliate_product_selections SET is_active = false WHERE affiliate_id = $1 AND product_id = $2"
            )
            .bind(aff_id).bind(prod_id)
            .execute(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"unselected": true})))
        }

        // ── Communications ──
        "comms.send" => {
            let body = data.ok_or("data required")?;
            let channel = body.get("channel").and_then(|v| v.as_str()).unwrap_or("email");
            let to = body.get("to").and_then(|v| v.as_str()).ok_or("to required")?;
            let subject = body.get("subject").and_then(|v| v.as_str());
            let body_text = body.get("body").and_then(|v| v.as_str()).ok_or("body required")?;
            let msg_id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO outbound_messages (id, tenant_id, channel, to_address, subject, body) VALUES ($1, $2, $3, $4, $5, $6)"
            )
            .bind(msg_id).bind(tenant_id).bind(channel).bind(to).bind(subject).bind(body_text)
            .execute(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((201, json!({"message_id": msg_id, "queued": true})))
        }

        // ── Events ──
        "events.ingest" => {
            let body = data.ok_or("data required")?;
            let source = body.get("source").and_then(|v| v.as_str()).unwrap_or("webhook");
            let event_type = body.get("event_type").and_then(|v| v.as_str()).ok_or("event_type required")?;
            let payload = body.get("payload").cloned().unwrap_or(json!({}));
            sqlx::query(
                "INSERT INTO events (id, tenant_id, source, event_type, payload) VALUES ($1, $2, $3, $4, $5)"
            )
            .bind(Uuid::new_v4()).bind(tenant_id).bind(source).bind(event_type).bind(payload)
            .execute(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((201, json!({"ingested": true})))
        }

        // ── AI ──
        "ai.assess" => {
            let contact_id = params.and_then(|p| p.get("contact_id").and_then(|v| v.as_str()))
                .ok_or("contact_id required")?;
            let cid = Uuid::parse_str(contact_id).map_err(|_| "invalid uuid".to_string())?;
            // Return basic score info from DB (full AI assessment requires LLM call)
            let score = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT * FROM scores WHERE contact_id = $1 AND tenant_id = $2"
            )
            .bind(cid).bind(tenant_id)
            .fetch_optional(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"churn_assessment": score})))
        }

        // ── Native Apps ──
        "native.connect" => {
            let body = data.ok_or("data required")?;
            let app_slug = body.get("app_slug").and_then(|v| v.as_str()).ok_or("app_slug required")?;
            let credentials = body.get("credentials").ok_or("credentials required")?;
            // Just store the connection intent; actual test happens in the connector
            sqlx::query(
                "INSERT INTO app_connections (id, tenant_id, app_slug, credentials, status) VALUES ($1, $2, $3, $4, 'connected') ON CONFLICT (tenant_id, app_slug) DO UPDATE SET credentials = $4, status = 'connected', updated_at = NOW()"
            )
            .bind(Uuid::new_v4()).bind(tenant_id).bind(app_slug).bind(credentials)
            .execute(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"connected": true, "app": app_slug})))
        }
        "native.sync.push" => {
            let body = data.ok_or("data required")?;
            let app_slug = body.get("app_slug").and_then(|v| v.as_str()).ok_or("app_slug required")?;
            let entity_type = body.get("entity_type").and_then(|v| v.as_str()).ok_or("entity_type required")?;
            let payload = body.get("payload").ok_or("payload required")?;
            let log_id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO app_sync_logs (id, tenant_id, app_slug, direction, entity_type, status, records_processed, completed_at) VALUES ($1, $2, $3, 'push', $4, 'completed', 1, NOW())"
            )
            .bind(log_id).bind(tenant_id).bind(app_slug).bind(entity_type)
            .execute(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"sync_id": log_id, "pushed": true, "data": payload})))
        }
        "native.sync.pull" => {
            let body = data.ok_or("data required")?;
            let app_slug = body.get("app_slug").and_then(|v| v.as_str()).ok_or("app_slug required")?;
            let entity_type = body.get("entity_type").and_then(|v| v.as_str()).ok_or("entity_type required")?;
            let log_id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO app_sync_logs (id, tenant_id, app_slug, direction, entity_type, status, records_processed, completed_at) VALUES ($1, $2, $3, 'pull', $4, 'completed', 0, NOW())"
            )
            .bind(log_id).bind(tenant_id).bind(app_slug).bind(entity_type)
            .execute(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"sync_id": log_id, "pull_requested": true})))
        }

        // ── Billing ──
        "billing.plans" => {
            let plans = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT id, name, slug, description, price_monthly, price_yearly, features, sort_order FROM plans WHERE is_active = true ORDER BY sort_order"
            )
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"plans": plans})))
        }
        "billing.credits" => {
            let credits = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT credits_remaining, credits_used, plan_name FROM v_credit_summary WHERE tenant_id = $1"
            )
            .bind(tenant_id)
            .fetch_optional(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"credits": credits})))
        }

        // ── Automation ──
        "automation.list" => {
            let rules = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT id, name, trigger_type, action_type, is_active, created_at FROM automation_rules WHERE tenant_id = $1 ORDER BY name"
            )
            .bind(tenant_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"automation_rules": rules})))
        }

        // ═══════════════════════════════════════════
        // Directory — Multi-Directory App webhooks
        // ═══════════════════════════════════════════

        // directory.listings — List business listings with filters
        "directory.listings" => {
            let limit = params.and_then(|p| p.get("limit").and_then(|v| v.as_i64())).unwrap_or(50);
            let offset = params.and_then(|p| p.get("offset").and_then(|v| v.as_i64())).unwrap_or(0);
            let unit = params.and_then(|p| p.get("unit").and_then(|v| v.as_str()));
            let state_filter = params.and_then(|p| p.get("state").and_then(|v| v.as_str()));

            let mut sql = String::from(
                "SELECT bp.id, bp.business_name, bp.unit, bp.current_state, bp.subscription_active, "
            );
            sql.push_str("bp.last_activity_at, bp.created_at, u.email, u.phone, u.first_name, u.last_name FROM business_profiles bp JOIN users u ON u.id = bp.user_id WHERE u.tenant_id = $1");

            let mut binds: Vec<String> = vec![];
            let mut param_idx = 2;
            if let Some(u) = unit {
                sql.push_str(&format!(" AND bp.unit = ${}", param_idx));
                binds.push(u.to_string());
                param_idx += 1;
            }
            if let Some(s) = state_filter {
                sql.push_str(&format!(" AND bp.current_state = ${}", param_idx));
                binds.push(s.to_string());
                param_idx += 1;
            }
            sql.push_str(&format!(" ORDER BY bp.last_activity_at DESC NULLS LAST LIMIT ${} OFFSET ${}", param_idx, param_idx + 1));

            // We need a dynamic query builder — use sqlx::query_as with the raw SQL and bind each param
            // For simplicity with variable bind counts, we fetch raw rows
            let mut query = sqlx::query_as::<_, (serde_json::Value,)>(&sql)
                .bind(tenant_id);
            for b in &binds {
                query = query.bind(b);
            }
            query = query.bind(limit as i32).bind(offset as i32);

            let listings = query.fetch_all(db).await
                .map_err(|e| format!("DB error: {}", e))?;

            // Also return total count
            let mut count_sql = String::from(
                "SELECT COUNT(*) as cnt FROM business_profiles bp JOIN users u ON u.id = bp.user_id WHERE u.tenant_id = $1"
            );
            if let Some(u) = unit {
                count_sql.push_str(&format!(" AND bp.unit = '{}'", u));
            }
            if let Some(s) = state_filter {
                count_sql.push_str(&format!(" AND bp.current_state = '{}'", s));
            }
            let total: (i64,) = sqlx::query_as(&count_sql)
                .bind(tenant_id)
                .fetch_one(db).await
                .map_err(|e| format!("DB error: {}", e))?;

            Ok((200, json!({
                "listings": listings,
                "total": total.0,
                "limit": limit,
                "offset": offset
            })))
        }

        // directory.listings.create — Create a new business listing
        "directory.listings.create" => {
            let body = data.ok_or("data required")?;
            let business_name = body.get("business_name").and_then(|v| v.as_str())
                .ok_or("business_name required")?;
            let unit = body.get("unit").and_then(|v| v.as_str())
                .unwrap_or("directory");
            let user_id_str = body.get("user_id").and_then(|v| v.as_str())
                .ok_or("user_id required")?;

            let user_id = Uuid::parse_str(user_id_str).map_err(|_| "invalid user_id".to_string())?;
            let profile_id = Uuid::new_v4();
            let state = body.get("current_state").and_then(|v| v.as_str()).unwrap_or("lead_captured");

            sqlx::query(
                "INSERT INTO business_profiles (id, user_id, business_name, unit, current_state) VALUES ($1, $2, $3, $4, $5)"
            )
            .bind(profile_id).bind(user_id).bind(business_name).bind(unit).bind(state)
            .execute(db).await
            .map_err(|e| format!("DB error: {}", e))?;

            // Log the creation event
            let _ = sqlx::query(
                "INSERT INTO event_logs (id, business_profile_id, event_name, metadata) VALUES ($1, $2, $3, $4)"
            )
            .bind(Uuid::new_v4())
            .bind(profile_id)
            .bind("directory.listing.created")
            .bind(json!({"business_name": business_name, "unit": unit}))
            .execute(db).await;

            Ok((201, json!({"id": profile_id, "business_name": business_name, "created": true})))
        }

        // directory.listings.get — Get listing details
        "directory.listings.get" => {
            let id = params.and_then(|p| p.get("id").and_then(|v| v.as_str()))
                .ok_or("listing id (business_profile_id) required")?;
            let profile_id = Uuid::parse_str(id).map_err(|_| "invalid uuid".to_string())?;

            let listing = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT bp.*, u.email, u.phone, u.first_name, u.last_name, u.name as user_name FROM business_profiles bp JOIN users u ON u.id = bp.user_id WHERE bp.id = $1 AND u.tenant_id = $2"
            )
            .bind(profile_id).bind(tenant_id)
            .fetch_optional(db).await
            .map_err(|e| format!("DB error: {}", e))?
            .ok_or("listing not found".to_string())?;

            // Also grab recent event logs
            let events = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT id, event_name, metadata, created_at FROM event_logs WHERE business_profile_id = $1 ORDER BY created_at DESC LIMIT 20"
            )
            .bind(profile_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;

            Ok((200, json!({
                "listing": listing,
                "recent_events": events
            })))
        }

        // directory.listings.update — Update a listing
        "directory.listings.update" => {
            let body = data.ok_or("data required")?;
            let id = params.and_then(|p| p.get("id").and_then(|v| v.as_str()))
                .or_else(|| body.get("id").and_then(|v| v.as_str()))
                .ok_or("listing id required")?;
            let profile_id = Uuid::parse_str(id).map_err(|_| "invalid uuid".to_string())?;

            // Build dynamic UPDATE for provided fields
            let mut sets: Vec<String> = vec![];
            let mut param_idx = 1;

            if let Some(_name) = body.get("business_name").and_then(|v| v.as_str()) {
                sets.push(format!("business_name = ${}", param_idx));
                param_idx += 1;
            }
            if let Some(_state) = body.get("current_state").and_then(|v| v.as_str()) {
                sets.push(format!("current_state = ${}", param_idx));
                param_idx += 1;
            }
            if let Some(_sub) = body.get("subscription_active").and_then(|v| v.as_bool()) {
                sets.push(format!("subscription_active = ${}", param_idx));
                param_idx += 1;
            }
            if let Some(_stripe) = body.get("stripe_customer_id").and_then(|v| v.as_str()) {
                sets.push(format!("stripe_customer_id = ${}", param_idx));
                param_idx += 1;
            }

            if sets.is_empty() {
                return Err("no fields to update".to_string());
            }

            sets.push("updated_at = NOW()".to_string());

            let sql = format!(
                "UPDATE business_profiles SET {} WHERE id = ${}",
                sets.join(", "),
                param_idx
            );

            let mut query = sqlx::query(&sql);
            if let Some(name) = body.get("business_name").and_then(|v| v.as_str()) {
                query = query.bind(name);
            }
            if let Some(state) = body.get("current_state").and_then(|v| v.as_str()) {
                query = query.bind(state);
            }
            if let Some(sub) = body.get("subscription_active").and_then(|v| v.as_bool()) {
                query = query.bind(sub);
            }
            if let Some(stripe) = body.get("stripe_customer_id").and_then(|v| v.as_str()) {
                query = query.bind(stripe);
            }
            query = query.bind(profile_id);

            query.execute(db).await
                .map_err(|e| format!("DB error: {}", e))?;

            // Log the update event
            let _ = sqlx::query(
                "INSERT INTO event_logs (id, business_profile_id, event_name, metadata) VALUES ($1, $2, $3, $4)"
            )
            .bind(Uuid::new_v4())
            .bind(profile_id)
            .bind("directory.listing.updated")
            .bind(json!({"updated_fields": sets}))
            .execute(db).await;

            Ok((200, json!({"id": profile_id, "updated": true})))
        }

        // directory.reviews — Pull reviews for a listing
        "directory.reviews" => {
            let id = params.and_then(|p| p.get("id").and_then(|v| v.as_str()))
                .ok_or("listing id (business_profile_id) required")?;
            let profile_id = Uuid::parse_str(id).map_err(|_| "invalid uuid".to_string())?;

            // Reviews are stored as event_logs with event_name = 'review.*'
            let reviews = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT id, event_name, metadata, created_at FROM event_logs WHERE business_profile_id = $1 AND event_name LIKE 'review.%' ORDER BY created_at DESC LIMIT 50"
            )
            .bind(profile_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;

            // Also look for any review-like metadata in the listing's prepopulated_data
            let prepopulated = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT data, preview_link, source_url FROM prepopulated_data  WHERE entity_id = $1 AND entity_type = 'business_profile' AND data ->> 'review' IS NOT NULL  ORDER BY created_at DESC LIMIT 10"
            )
            .bind(profile_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;

            Ok((200, json!({
                "reviews": reviews,
                "external_reviews": prepopulated,
                "total_reviews": reviews.len()
            })))
        }

        // directory.followups — Check followup status for a listing
        "directory.followups" => {
            let id = params.and_then(|p| p.get("id").and_then(|v| v.as_str()))
                .ok_or("listing id (business_profile_id) required")?;
            let profile_id = Uuid::parse_str(id).map_err(|_| "invalid uuid".to_string())?;

            let pending = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT id, scheduled_for, channel, template_slug, created_at FROM followup_queue  WHERE business_profile_id = $1 AND is_executed = false AND is_cancelled = false  ORDER BY scheduled_for ASC"
            )
            .bind(profile_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;

            let executed = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT id, scheduled_for, channel, template_slug, executed_at, created_at FROM followup_queue  WHERE business_profile_id = $1 AND is_executed = true  ORDER BY executed_at DESC NULLS LAST LIMIT 50"
            )
            .bind(profile_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;

            // Also pull checklist progress if any
            let checklist = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT ci.id, ct.name as template_name, ci.current_stage, ci.completed, ci.started_at, ci.completed_at  FROM checklist_instances ci  JOIN checklist_templates ct ON ct.id = ci.template_id  WHERE ci.tenant_id = $1 AND ci.entity_id = $2  ORDER BY ci.created_at DESC LIMIT 5"
            )
            .bind(tenant_id).bind(profile_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;

            Ok((200, json!({
                "pending_followups": pending,
                "executed_followups": executed,
                "checklist_instances": checklist
            })))
        }

        // directory.analytics — Get directory analytics
        "directory.analytics" => {
            let unit = params.and_then(|p| p.get("unit").and_then(|v| v.as_str()));

            // Total listings by state (summary)
            let state_breakdown = if let Some(u) = unit {
                sqlx::query_as::<_, (serde_json::Value,)>(
                    "SELECT current_state, COUNT(*) as count FROM business_profiles bp  JOIN users u2 ON u2.id = bp.user_id  WHERE u2.tenant_id = $1 AND bp.unit = $2  GROUP BY current_state ORDER BY count DESC"
                )
                .bind(tenant_id).bind(u)
                .fetch_all(db).await
            } else {
                sqlx::query_as::<_, (serde_json::Value,)>(
                    "SELECT bp.current_state, COUNT(*) as count FROM business_profiles bp  JOIN users u2 ON u2.id = bp.user_id  WHERE u2.tenant_id = $1  GROUP BY bp.current_state ORDER BY count DESC"
                )
                .bind(tenant_id)
                .fetch_all(db).await
            }.map_err(|e| format!("DB error: {}", e))?;

            // Listings by unit
            let unit_breakdown = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT bp.unit, COUNT(*) as count FROM business_profiles bp  JOIN users u2 ON u2.id = bp.user_id  WHERE u2.tenant_id = $1  GROUP BY bp.unit ORDER BY count DESC"
            )
            .bind(tenant_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;

            // Recent event volume (last 30 days)
            let event_volume = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT el.event_name, DATE(el.created_at) as day, COUNT(*) as count  FROM event_logs el  JOIN business_profiles bp ON bp.id = el.business_profile_id  JOIN users u2 ON u2.id = bp.user_id  WHERE u2.tenant_id = $1 AND el.created_at > NOW() - INTERVAL '30 days'  GROUP BY el.event_name, DATE(el.created_at)  ORDER BY day DESC, count DESC LIMIT 100"
            )
            .bind(tenant_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;

            // Followup queue stats
            let followup_stats = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT  COUNT(*) FILTER (WHERE is_executed = false AND is_cancelled = false) as pending,  COUNT(*) FILTER (WHERE is_executed = true) as executed,  COUNT(*) FILTER (WHERE is_cancelled = true) as cancelled  FROM followup_queue fq  JOIN business_profiles bp ON bp.id = fq.business_profile_id  JOIN users u2 ON u2.id = bp.user_id  WHERE u2.tenant_id = $1"
            )
            .bind(tenant_id)
            .fetch_one(db).await
            .map_err(|e| format!("DB error: {}", e))?;

            // Subscription stats
            let subscription_stats = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT subscription_active, COUNT(*) as count FROM business_profiles bp  JOIN users u2 ON u2.id = bp.user_id  WHERE u2.tenant_id = $1  GROUP BY subscription_active"
            )
            .bind(tenant_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;

            Ok((200, json!({
                "state_breakdown": state_breakdown,
                "unit_breakdown": unit_breakdown,
                "event_volume_30d": event_volume,
                "followup_stats": followup_stats,
                "subscription_stats": subscription_stats
            })))
        }

        // directory.health — Check directory system health
        "directory.health" => {
            // 1. Total profiles
            let total_profiles: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM business_profiles bp  JOIN users u2 ON u2.id = bp.user_id WHERE u2.tenant_id = $1"
            )
            .bind(tenant_id)
            .fetch_one(db).await
            .map_err(|e| format!("DB error: {}", e))?;

            // 2. Event logs count (recent 24h)
            let events_24h: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM event_logs el  JOIN business_profiles bp ON bp.id = el.business_profile_id  JOIN users u2 ON u2.id = bp.user_id  WHERE u2.tenant_id = $1 AND el.created_at > NOW() - INTERVAL '24 hours'"
            )
            .bind(tenant_id)
            .fetch_one(db).await
            .map_err(|e| format!("DB error: {}", e))?;

            // 3. Pending followups (stale checks)
            let stale_followups: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM followup_queue fq  JOIN business_profiles bp ON bp.id = fq.business_profile_id  JOIN users u2 ON u2.id = bp.user_id  WHERE u2.tenant_id = $1 AND fq.is_executed = false AND fq.is_cancelled = false AND fq.scheduled_for < NOW() - INTERVAL '1 hour'"
            )
            .bind(tenant_id)
            .fetch_one(db).await
            .map_err(|e| format!("DB error: {}", e))?;

            // 4. Profiles stuck in 'lead_captured' for > 7 days
            let stuck_leads: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM business_profiles bp  JOIN users u2 ON u2.id = bp.user_id  WHERE u2.tenant_id = $1 AND bp.current_state = 'lead_captured' AND bp.created_at < NOW() - INTERVAL '7 days'"
            )
            .bind(tenant_id)
            .fetch_one(db).await
            .map_err(|e| format!("DB error: {}", e))?;

            // 5. Recent errors logged as events
            let recent_errors = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT el.id, el.event_name, el.metadata, el.created_at FROM event_logs el  JOIN business_profiles bp ON bp.id = el.business_profile_id  JOIN users u2 ON u2.id = bp.user_id  WHERE u2.tenant_id = $1 AND el.event_name LIKE 'error.%'  ORDER BY el.created_at DESC LIMIT 20"
            )
            .bind(tenant_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;

            Ok((200, json!({
                "status": "ok",
                "total_profiles": total_profiles.0,
                "events_24h": events_24h.0,
                "stale_followups": stale_followups.0,
                "stuck_leads_7d": stuck_leads.0,
                "recent_errors": recent_errors,
                "tables": ["business_profiles", "event_logs", "followup_queue"]
            })))
        }

        // ── Tenant Management (for FunnelSwift auto-provisioning) ──
        "tenants.create" => {
            let body = data.ok_or("data required: name, email")?;
            let name = body.get("name").and_then(|v| v.as_str()).ok_or("name required")?;
            let email = body.get("email").and_then(|v| v.as_str()).ok_or("email required")?;

            // Create tenant
            let slug = format!("{}-{}", name.to_lowercase().replace(' ', "-"), &Uuid::new_v4().to_string()[..8]);
            let new_tid = Uuid::new_v4();
            sqlx::query("INSERT INTO tenants (id, name, slug) VALUES ($1, $2, $3)")
                .bind(new_tid).bind(name).bind(&slug)
                .execute(db).await
                .map_err(|e| format!("Failed to create tenant: {}", e))?;

            // Create owner user with temp password
            let uid = Uuid::new_v4();
            use argon2::password_hash::{SaltString, PasswordHasher};
            let salt = SaltString::generate(&mut rand::thread_rng());
            let temp_pass = format!("temp-{}", &Uuid::new_v4().to_string()[..8]);
            let pw_hash = argon2::Argon2::default()
                .hash_password(temp_pass.as_bytes(), &salt)
                .map_err(|e| format!("Hash error: {}", e))?
                .to_string();

            sqlx::query(
                "INSERT INTO users (id, tenant_id, email, password_hash, name, role) VALUES ($1, $2, $3, $4, $5, 'account_owner')"
            )
            .bind(uid).bind(new_tid).bind(email).bind(&pw_hash).bind(name)
            .execute(db).await
            .map_err(|e| format!("Failed to create user: {}", e))?;

            // Assign free plan
            let plan = sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM plans WHERE slug = 'free' LIMIT 1"
            )
            .fetch_optional(db).await
            .map_err(|e| format!("DB error: {}", e))?;

            if let Some(plan_id) = plan {
                let _ = sqlx::query(
                    "INSERT INTO tenant_plans (id, tenant_id, plan_id, status, billing_cycle) VALUES ($1, $2, $3, 'active', 'monthly') ON CONFLICT (tenant_id) DO NOTHING"
                )
                .bind(Uuid::new_v4()).bind(new_tid).bind(plan_id)
                .execute(db).await;
            }

            // Create affiliate profile
            let code = format!("{}{}", name.to_lowercase().chars().filter(|c| c.is_alphanumeric()).take(6).collect::<String>(),
                rand::thread_rng().gen_range(100..999));
            sqlx::query(
                "INSERT INTO affiliates (id, tenant_id, user_id, code, commission_rate, commission_type) VALUES ($1, $2, $3, $4, '10', 'percentage') ON CONFLICT (tenant_id, user_id) DO NOTHING"
            )
            .bind(Uuid::new_v4()).bind(new_tid).bind(uid).bind(&code)
            .execute(db).await
            .map_err(|e| format!("Failed to create affiliate: {}", e))?;

            Ok((201, json!({
                "tenant_id": new_tid,
                "user_id": uid,
                "slug": slug,
                "email": email,
                "temp_password": temp_pass,
                "affiliate_code": code,
                "plan": "free",
                "auto_webhook_token": true
            })))
        }

        // ── Webhook Token Management ──
        "webhooks.generate" => {
            let token = Uuid::new_v4().to_string();
            let action_list: Vec<String> = vec![]; // start with empty; token can be updated later
            let wh_id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO automation_webhooks (id, tenant_id, name, webhook_token, allowed_actions, is_active) VALUES ($1, $2, $3, $4, $5, true)"
            )
            .bind(wh_id).bind(tenant_id).bind("Webhook token").bind(&token).bind(&action_list)
            .execute(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((201, json!({"id": wh_id, "webhook_token": token, "allowed_actions": action_list, "created": true})))
        }
        "webhooks.revoke" => {
            let body = data.ok_or("data required")?;
            if let Some(wid) = body.get("webhook_id").and_then(|v| v.as_str()) {
                let uid = Uuid::parse_str(wid).map_err(|_| "invalid webhook_id".to_string())?;
                sqlx::query(
                    "UPDATE automation_webhooks SET is_active = false, updated_at = NOW() WHERE id = $1 AND tenant_id = $2"
                )
                .bind(uid).bind(tenant_id)
                .execute(db).await
                .map_err(|e| format!("DB error: {}", e))?;
            } else if let Some(tok) = body.get("token").and_then(|v| v.as_str()) {
                sqlx::query(
                    "UPDATE automation_webhooks SET is_active = false, updated_at = NOW() WHERE webhook_token = $1 AND tenant_id = $2"
                )
                .bind(tok).bind(tenant_id)
                .execute(db).await
                .map_err(|e| format!("DB error: {}", e))?;
            } else {
                return Err("webhook_id or token required".to_string());
            }
            Ok((200, json!({"revoked": true})))
        }
        "webhooks.list" => {
            let webhooks = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT id, name,\n  CONCAT(LEFT(webhook_token, 4), REPEAT('*', GREATEST(0, LENGTH(webhook_token) - 8)), RIGHT(webhook_token, 4)) as masked_token,\n  allowed_actions, created_at, last_used_at, is_active\n FROM automation_webhooks WHERE tenant_id = $1 ORDER BY created_at DESC"
            )
            .bind(tenant_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"webhooks": webhooks})))
        }

        // ── Pipeline Stages ──
        "pipelines.stages" => {
            let pipeline_id = params.and_then(|p| p.get("pipeline_id").and_then(|v| v.as_str()))
                .ok_or("pipeline_id required")?;
            let pid = Uuid::parse_str(pipeline_id).map_err(|_| "invalid uuid".to_string())?;
            let stages = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT id, pipeline_id, name, sort_order, color, created_at FROM pipeline_stages WHERE pipeline_id = $1 ORDER BY sort_order"
            )
            .bind(pid)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"stages": stages})))
        }
        "pipelines.create_stage" => {
            let body = data.ok_or("data required")?;
            let pipeline_id = body.get("pipeline_id").and_then(|v| v.as_str())
                .ok_or("pipeline_id required")?;
            let name = body.get("name").and_then(|v| v.as_str())
                .ok_or("name required")?;
            let sort_order = body.get("sort_order").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
            let color = body.get("color").and_then(|v| v.as_str()).unwrap_or("#CCCCCC");
            let pid = Uuid::parse_str(pipeline_id).map_err(|_| "invalid pipeline_id".to_string())?;
            let stage_id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO pipeline_stages (id, pipeline_id, name, sort_order, color) VALUES ($1, $2, $3, $4, $5)"
            )
            .bind(stage_id).bind(pid).bind(name).bind(sort_order).bind(color)
            .execute(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((201, json!({"id": stage_id, "created": true})))
        }

        // ── Users ──
        "users.invite" => {
            let body = data.ok_or("data required")?;
            let email = body.get("email").and_then(|v| v.as_str())
                .ok_or("email required")?;
            let name = body.get("name").and_then(|v| v.as_str())
                .ok_or("name required")?;
            let role = body.get("role").and_then(|v| v.as_str()).unwrap_or("member");
            if role != "member" && role != "admin" {
                return Err("role must be 'member' or 'admin'".to_string());
            }
            let uid = Uuid::new_v4();
            let temp_password = format!("temp-{}", &Uuid::new_v4().to_string()[..8]);
            // Use a placeholder password hash; the invited user must reset on first login
            sqlx::query(
                "INSERT INTO users (id, tenant_id, email, password_hash, name, role, temp_password, is_active) VALUES ($1, $2, $3, $4, $5, $6, $7, true)"
            )
            .bind(uid).bind(tenant_id).bind(email).bind("PLACEHOLDER_HASH").bind(name).bind(role).bind(&temp_password)
            .execute(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((201, json!({"id": uid, "email": email, "name": name, "role": role, "temp_password": temp_password, "invited": true})))
        }
        "users.list" => {
            let users = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT id, email, name, role, is_active, created_at FROM users WHERE tenant_id = $1 ORDER BY created_at"
            )
            .bind(tenant_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"users": users})))
        }

        // ── Tenants ──
        "tenants.settings" => {
            let tenant = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT id, name, slug, NULL::text AS plan, NULL::uuid AS plan_id FROM tenants WHERE id = $1"
            )
            .bind(tenant_id)
            .fetch_optional(db).await
            .map_err(|e| format!("DB error: {}", e))?
            .ok_or("tenant not found".to_string())?;
            let webhook_count: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM automation_webhooks WHERE tenant_id = $1"
            )
            .bind(tenant_id)
            .fetch_one(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            let plan_info = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT p.id as plan_id, p.name, p.slug, p.price_monthly, p.price_yearly, tp.status, tp.billing_cycle\n FROM tenant_plans tp JOIN plans p ON p.id = tp.plan_id WHERE tp.tenant_id = $1"
            )
            .bind(tenant_id)
            .fetch_optional(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({
                "account": tenant,
                "webhook_token_count": webhook_count.0,
                "plan": plan_info
            })))
        }

        // ── Scoring ──
        "scoring.calculate" => {
            let body = data.ok_or("data required")?;
            let contact_id = body.get("contact_id").and_then(|v| v.as_str())
                .ok_or("contact_id required")?;
            let cid = Uuid::parse_str(contact_id).map_err(|_| "invalid contact_id".to_string())?;
            // Read contact fields for scoring
            let contact = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT id, first_name, last_name, email, phone, company_id, score FROM contacts WHERE id = $1 AND tenant_id = $2"
            )
            .bind(cid).bind(tenant_id)
            .fetch_optional(db).await
            .map_err(|e| format!("DB error: {}", e))?
            .ok_or("contact not found".to_string())?;
            // Simple scoring: base 50 + 10 if has email + 10 if has phone + 10 if has company_id + 20 if existing score > 0
            let has_email = contact.0.get("email").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
            let has_phone = contact.0.get("phone").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
            let has_company = contact.0.get("company_id").and_then(|v| v.as_str()).map(|s| !s.is_empty()).unwrap_or(false);
            let existing_score = contact.0.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let mut score: i32 = 50;
            if has_email { score += 10; }
            if has_phone { score += 10; }
            if has_company { score += 10; }
            if existing_score > 0.0 { score += 20; }
            sqlx::query(
                "UPDATE contacts SET score = $1, updated_at = NOW() WHERE id = $2 AND tenant_id = $3"
            )
            .bind(score).bind(cid).bind(tenant_id)
            .execute(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"contact_id": cid, "score": score, "calculated": true})))
        }

        // ── Analytics ──
        "analytics.contacts" => {
            let total: (i64,) = sqlx::query_as(
                "SELECT COUNT(*) FROM contacts WHERE tenant_id = $1"
            )
            .bind(tenant_id)
            .fetch_one(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            // Contacts by tag
            let by_tag = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT t.id, t.name, COUNT(ta.entity_id) as count\n FROM tags t\n LEFT JOIN tag_assignments ta ON ta.tag_id = t.id AND ta.entity_type = 'contact'\n WHERE t.tenant_id = $1\n GROUP BY t.id, t.name ORDER BY count DESC"
            )
            .bind(tenant_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            // Contacts created over the last 30 days, grouped by day
            let by_day = sqlx::query_as::<_, (serde_json::Value,)>(
                "SELECT DATE(created_at) as day, COUNT(*) as count\n FROM contacts\n WHERE tenant_id = $1 AND created_at > NOW() - INTERVAL '30 days'\n GROUP BY DATE(created_at) ORDER BY day"
            )
            .bind(tenant_id)
            .fetch_all(db).await
            .map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({
                "total_contacts": total.0,
                "by_tag": by_tag,
                "created_over_time_30d": by_day
            })))
        }

        // ── Audit ──
        "audit.log" => {
            let limit = params.and_then(|p| p.get("limit").and_then(|v| v.as_i64())).unwrap_or(50);
            let entity_filter = params.and_then(|p| p.get("entity_type").and_then(|v| v.as_str()));
            let entries = if let Some(entity_type) = entity_filter {
                sqlx::query_as::<_, (serde_json::Value,)>(
                    "SELECT id, entity_type, entity_id, action, performed_by, metadata, created_at\n FROM audit_logs\n WHERE tenant_id = $1 AND entity_type = $2\n ORDER BY created_at DESC LIMIT $3"
                )
                .bind(tenant_id).bind(entity_type).bind(limit as i32)
                .fetch_all(db).await
            } else {
                sqlx::query_as::<_, (serde_json::Value,)>(
                    "SELECT id, entity_type, entity_id, action, performed_by, metadata, created_at\n FROM audit_logs\n WHERE tenant_id = $1\n ORDER BY created_at DESC LIMIT $2"
                )
                .bind(tenant_id).bind(limit as i32)
                .fetch_all(db).await
            }.map_err(|e| format!("DB error: {}", e))?;
            Ok((200, json!({"audit_log": entries})))
        }

        // ── Search ──
        "search.query" => {
            let q = params.and_then(|p| p.get("q").and_then(|v| v.as_str()))
                .ok_or("search term 'q' required")?;
            let allowed = ["contacts", "tags", "lists"];
            let entities_param = params.and_then(|p| p.get("entities").and_then(|v| v.as_array()));
            let entities: Vec<&str> = if let Some(arr) = entities_param {
                arr.iter().filter_map(|v| v.as_str()).filter(|e| allowed.contains(e)).collect()
            } else {
                allowed.to_vec()
            };
            let pattern = format!("%{}%", q);
            let mut results = serde_json::Map::new();
            if entities.contains(&"contacts") {
                let contacts = sqlx::query_as::<_, (serde_json::Value,)>(
                    "SELECT id, first_name, last_name, email, phone, score FROM contacts\n WHERE tenant_id = $1 AND (email ILIKE $2 OR first_name ILIKE $2 OR last_name ILIKE $2 OR CONCAT(first_name, ' ', last_name) ILIKE $2)\n LIMIT 20"
                )
                .bind(tenant_id).bind(&pattern)
                .fetch_all(db).await
                .map_err(|e| format!("DB error: {}", e))?;
                results.insert("contacts".to_string(), json!(contacts));
            }
            if entities.contains(&"tags") {
                let tags = sqlx::query_as::<_, (serde_json::Value,)>(
                    "SELECT id, name, color FROM tags WHERE tenant_id = $1 AND name ILIKE $2 LIMIT 20"
                )
                .bind(tenant_id).bind(&pattern)
                .fetch_all(db).await
                .map_err(|e| format!("DB error: {}", e))?;
                results.insert("tags".to_string(), json!(tags));
            }
            if entities.contains(&"lists") {
                let lists = sqlx::query_as::<_, (serde_json::Value,)>(
                    "SELECT id, name, list_type FROM lists WHERE tenant_id = $1 AND name ILIKE $2 LIMIT 20"
                )
                .bind(tenant_id).bind(&pattern)
                .fetch_all(db).await
                .map_err(|e| format!("DB error: {}", e))?;
                results.insert("lists".to_string(), json!(lists));
            }
            Ok((200, json!({"results": results, "query": q})))
        }

        _ => Err(format!("Unknown action: {}", action)),
    };

    let elapsed = start.elapsed().as_millis() as i64;

    match result {
        Ok((status, data)) => Ok((status, {
            let mut d = data;
            if let Some(obj) = d.as_object_mut() {
                obj.insert("elapsed_ms".to_string(), json!(elapsed));
            }
            d
        })),
        Err(e) => Err(e),
    }
}
