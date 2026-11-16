//! `export` — `GET /api/v1/account/export`: the customer's OWN copy of their workspace, one file.
//!
//! Parity with the rest of the fleet (FunnelSwift, WorkflowSwift, ADASwift, IncentiveSwift and
//! MissedCall all answer this path). CoreSwift could hand out two CSV tables and nothing else —
//! `/api/csv/export/contacts` and `/api/csv/export/opportunities` — so a workspace leaving could
//! not take its pipelines, tickets, bookings, automations or mail with it.
//!
//! # Scope
//!
//! The account comes from the TOKEN (`claims.aid`), never from a path segment, a query string or a
//! body: there is nothing to spoof, and one workspace can only ever receive its own document. Every
//! collection is a literal `SELECT … WHERE tenant_id = $1` — explicit column lists, never
//! `SELECT *`, so a column added later cannot silently widen the file.
//!
//! # What is deliberately NOT in the file
//!
//! Nothing that could be replayed to sign in as somebody, and no provider secret:
//! `users.password_hash` / `users.sessions_valid_from`, `satellite_api_keys.key_hash`,
//! `webhook_endpoints.secret`, `integration_targets.api_key`, `tenant_invites.token`,
//! `booking_calendars.google_refresh_token`, `private_email_boxes.mailgun_mailbox_id`,
//! `tenants.settings` / `tenants.integration_config` (provider internals) and
//! `tenants.probe_harness` (the internal test marker). Each key's PREFIX and its dates ARE here.
//! The document states this in its own `excluded` field, in plain English.
//!
//! # Shape
//!
//! One pretty-printed JSON object served as an attachment:
//! `{ format, exported_at, counts:{…}, excluded, <collection>:[…] }`.

use axum::{
    extract::{Extension, State},
    http::{header, HeaderValue},
    response::{IntoResponse, Response},
};
use serde_json::{json, Map, Value};
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// Every collection in the export: the key it appears under, and the query that reads it.
///
/// `$1` is the caller's account id, taken from the token. Every table here carries its own
/// `tenant_id`, so no collection needs a sub-select to be scoped — one bind, one workspace.
/// Column lists are explicit and were measured against the live schema
/// (`information_schema.columns`) before this file was written.
const COLLECTIONS: &[(&str, &str)] = &[
    (
        "tenant",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, slug, primary_color, accent_color, custom_domain, branding_name,
                    is_active, industry_slug, created_at, updated_at
               FROM tenants WHERE id = $1) x",
    ),
    (
        "plan",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT tp.id, tp.plan_id, p.slug AS plan_slug, p.name AS plan_name, tp.status,
                    tp.billing_cycle, tp.trial_ends_at, tp.current_period_starts_at,
                    tp.current_period_ends_at, tp.credit_balance, tp.lifetime_credits,
                    tp.canceled_at, tp.created_at
               FROM tenant_plans tp
               LEFT JOIN plans p ON p.id = tp.plan_id
              WHERE tp.tenant_id = $1) x",
    ),
    (
        "users",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, email, name, first_name, last_name, role, username, company, phone,
                    avatar_url, is_active, last_login_at, password_changed_at, closed_at,
                    created_at, updated_at
               FROM users WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "workspaces",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, is_default, created_at, updated_at
               FROM workspaces WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "contacts",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, first_name, last_name, email, phone, company, job_title, title, city, state,
                    country, address_line1, address_line2, postal_code, gender, source, score,
                    score_category, is_active, last_contacted_at, company_id, notes, metadata,
                    created_at, updated_at
               FROM contacts WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "contact_custom_fields",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, key, label, field_type, source_app, is_active, created_at, updated_at
               FROM contact_custom_fields WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "companies",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, website, phone, email, industry, size, city, state, country,
                    address_line1, address_line2, postal_code, domain, description, notes,
                    metadata, is_active, is_archived, created_at, updated_at
               FROM companies WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "opportunities",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, pipeline_id, stage_id, contact_id, company_id, name, value, currency,
                    probability, expected_close_date, source, notes, metadata, is_won, is_lost,
                    lost_reason, created_at, updated_at
               FROM opportunities WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "pipelines",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, description, is_default, is_active, created_at, updated_at
               FROM pipelines WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "tags",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, category_id, name, color, parent_id, is_dynamic, description, is_active,
                    created_at, updated_at
               FROM tags WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "tag_categories",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, color, description, created_at
               FROM tag_categories WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "tag_assignments",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, tag_id, entity_type, entity_id, assigned_by, assigned_at
               FROM tag_assignments WHERE tenant_id = $1 ORDER BY assigned_at) x",
    ),
    (
        "lists",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, description, list_type, dynamic_rules, member_count, is_active,
                    created_at, updated_at
               FROM lists WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "list_members",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, list_id, contact_id, added_manually, added_at
               FROM list_members WHERE tenant_id = $1 ORDER BY added_at) x",
    ),
    (
        "events",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, title, description, event_date, start_time, end_time, location, event_type,
                    status, contact_id, company_id, deal_id, created_by, source, entity_type,
                    entity_id, created_at, updated_at
               FROM events WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "outbound_messages",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, contact_id, channel, recipient, to_address, cc_address, bcc_address,
                    subject, body, is_html, status, scheduled_at, sent_at, opened_at, clicked_at,
                    cancelled, error_message, retry_count, provider, created_at
               FROM outbound_messages WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "internal_messages",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, sender_id, recipient_id, subject, body, channel, status, read, direction,
                    thread_id, parent_message_id, is_archived, created_at, updated_at
               FROM cs_messages WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "message_templates",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, channel, subject, body, variables, created_at, updated_at
               FROM message_templates WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "email_campaigns",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, description, status, created_by, created_at, updated_at
               FROM email_campaigns WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "email_templates",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, template_type, name, subject, body, body_text, html_body, is_default,
                    created_at, updated_at
               FROM email_templates WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "tickets",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, number, subject, description, status, priority, assigned_to, contact_id,
                    contact_email, contact_name, source, custom_fields, csat_score,
                    first_responded_at, solved_at, created_at, updated_at
               FROM tickets WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "helpdesk_sites",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, slug, support_email, from_name, brand, business_hours,
                    allowed_origins, ai_enabled, ai_persona, default_queue_id, kb_autodraft,
                    created_at, updated_at
               FROM helpdesk_sites WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "ticket_inboxes",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, email_fwd, auto_created, created_at
               FROM ticket_inboxes WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "booking_calendars",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, slug, description, calendar_type, kind, timezone, currency,
                    hold_minutes, is_active, reply_to_email, send_confirmation, metadata,
                    created_at, updated_at
               FROM booking_calendars WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "slot_bookings",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, calendar_id, slot_id, contact_id, business_name, contact_name,
                    contact_email, contact_phone, website, description, target_audience,
                    call_booking, start_date, end_date, slot_position, status, price_paid,
                    currency, metadata, created_at, updated_at
               FROM slot_bookings WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "booking_orders",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, calendar_id, contact_id, customer_name, customer_email, status,
                    subtotal_cents, discount_cents, total_cents, bundle_id, hold_expires_at,
                    notes, created_at
               FROM booking_orders WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "scores",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, contact_id, total_score, category, last_event_type, last_event_at,
                    created_at, updated_at
               FROM scores WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "score_rules",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, event_type, description, points, direction, is_active, created_at,
                    updated_at
               FROM score_rules WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "automation_rules",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, description, trigger_type, trigger_config, action_type,
                    action_config, is_active, execution_count, last_executed_at, created_at,
                    updated_at
               FROM automation_rules WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "notification_rules",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, event_type, trigger_event, channel, action, target_entity, template_id,
                    config, is_active, created_at, updated_at
               FROM notification_rules WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "webhook_endpoints",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, url, events, retry_count, timeout_ms, is_active,
                    last_triggered_at, failure_count, created_at, updated_at
               FROM webhook_endpoints WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "integration_targets",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, portfolio_company_id, user_id, name, provider, webhook_url, events,
                    is_active, created_at, updated_at
               FROM integration_targets WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "integrations",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, provider, is_active, last_sync_at, created_at, updated_at
               FROM integrations WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "api_keys",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, user_id, name, source_app, key_prefix, permissions, is_active,
                    last_used_at, created_at, updated_at
               FROM satellite_api_keys WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "portfolio_companies",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, slug, email, description, settings, is_active, account_tenant_id,
                    created_at, updated_at
               FROM portfolio_companies WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "business_profiles",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, business_name, slug, category, description, address, unit,
                    current_state, phone, website, logo_url, is_claimed, subscription_active,
                    last_activity_at, created_at, updated_at
               FROM business_profiles WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "credit_transactions",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, action_type, credits, description, entity_type, entity_id, created_at
               FROM credit_transactions WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "tracked_links",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, tag_id, slug, target_url, created_at
               FROM tracked_links WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "link_clicks",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, tracked_link_id, contact_id, clicked_at
               FROM link_clicks WHERE tenant_id = $1 ORDER BY clicked_at) x",
    ),
    (
        "support_widgets",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, inbox_id, name, slug, theme_color, greeting, welcome_msg, position,
                    is_active, created_at
               FROM support_widgets WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "feedback",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, user_id, app, screen, kind, message, status, created_at
               FROM feedback WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "checklist_templates",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, description, trigger_type, stage_count, days_per_stage, is_active,
                    created_at, updated_at
               FROM checklist_templates WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "checklist_instances",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, template_id, entity_type, entity_id, current_stage, completed,
                    started_at, completed_at, created_at
               FROM checklist_instances WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "email_boxes",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, user_id, local_part, email_address, forwarding_enabled, status,
                    access_mode, creates_ticket, trial_started_at, trial_ends_at, created_at,
                    updated_at
               FROM private_email_boxes WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "email_folders",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, path, name, parent, created_at
               FROM private_email_folders WHERE tenant_id = $1 ORDER BY path) x",
    ),
    (
        "email_labels",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, name, color, created_at
               FROM private_email_labels WHERE tenant_id = $1 ORDER BY name) x",
    ),
    (
        "email_messages",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, mailbox_id, address, folder, direction, from_address, from_name,
                    to_address, cc_address, subject, snippet, body_plain, is_read, is_starred,
                    size_bytes, has_attachments, ticket_id, is_ticket, assigned_user_id,
                    received_at, created_at
               FROM private_email_messages WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "industries",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, user_id, industry_slug, dashboard_name, is_active, created_at, updated_at
               FROM industries WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
    (
        "email_signatures",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT full_name, job_title, company, phone, line, updated_at
               FROM tenant_email_signatures WHERE tenant_id = $1) x",
    ),
    (
        "team_invites",
        "SELECT coalesce(json_agg(x), '[]'::json)::text FROM (
             SELECT id, role, accepted, accepted_at, expires_at, created_at
               FROM tenant_invites WHERE tenant_id = $1 ORDER BY created_at) x",
    ),
];

/// What the customer must be told about their own copy: what is NOT in it, and why.
const EXCLUDED: &str = "Nothing that could be replayed to sign in as somebody or to act as this \
     workspace is included: stored passwords and password hashes, API-key secrets (each key's \
     PREFIX, its name and its dates are here), webhook signing secrets, connected-app credentials \
     and the Google calendar connection token, password-reset and email-confirmation tokens, \
     unopened team-invite links, payment references, and the internal test marker on the \
     workspace. CoreSwift's own operational logs (API usage, delivery retries, audit trail) are \
     not part of this file. Ask support if you need one of those in a different form.";

/// `GET /api/v1/account/export`
pub async fn export_account(
    Extension(claims): Extension<Claims>,
    State(state): State<AppState>,
) -> ApiResult<Response> {
    let tenant_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let mut out = Map::new();
    let mut counts = Map::new();

    for (key, sql) in COLLECTIONS {
        let raw: String = sqlx::query_scalar(*sql)
            .bind(tenant_id)
            .fetch_one(&state.db)
            .await?;
        // A NULL row-set would mean the query returned no aggregate at all, which
        // `coalesce` already rules out; anything unparseable degrades to an empty list rather
        // than failing the whole download.
        let rows: Value = serde_json::from_str(&raw).unwrap_or_else(|_| json!([]));
        counts.insert(
            (*key).to_string(),
            json!(rows.as_array().map(|a| a.len()).unwrap_or(0)),
        );
        out.insert((*key).to_string(), rows);
    }

    out.insert("counts".to_string(), Value::Object(counts));
    out.insert("excluded".to_string(), json!(EXCLUDED));
    out.insert(
        "exported_at".to_string(),
        json!(chrono::Utc::now().to_rfc3339()),
    );
    out.insert("format".to_string(), json!("coreswift-account-export/1"));

    let body = serde_json::to_string_pretty(&Value::Object(out))
        .map_err(|e| AppError::Internal(format!("export encode error: {e}")))?;

    let filename = format!(
        "coreswift-account-{}.json",
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    );
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{filename}\""))
        .map_err(|e| AppError::Internal(format!("export header error: {e}")))?;

    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            ),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        body,
    )
        .into_response())
}
