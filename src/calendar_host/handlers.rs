//! The hosted-calendar endpoints. Generation, uniqueness enforcement and Host resolution live in
//! `mod.rs`; these are the requests that read and change one workspace's address, plus the two
//! public ones the hosted page itself calls.

use crate::auth::models::Claims;
use crate::bookings::models::CreateBookingRequest;
use crate::errors::{ApiResult, AppError};
use crate::AppState;
use axum::{
    extract::{Extension, Json, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::{
    ensure_for_tenant, host_only, is_reserved, is_system_domain, is_unique_violation,
    normalise_domain, public_url, resolve_calendar_subdomain, resolve_host, suggest_subdomain,
    system_domain, taken_message_with_suggestion, valid_domain, valid_subdomain, CalendarHost,
};

/// Both `PATCH /api/calendar-host/subdomain` and the console's own save button are the ACCOUNT's
/// decision about where its calendar answers, so they demand an owner/admin token — the same bar the
/// rest of the account surface uses. A 'member' can read the address; only an owner changes it.
fn require_account_admin(c: &Claims) -> Result<Uuid, AppError> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    if c.role != "owner"
        && c.role != "account_owner"
        && c.role != "admin"
        && c.role != "agency_admin"
    {
        return Err(AppError::Forbidden);
    }
    Ok(tenant_id)
}

fn host_payload(host: &CalendarHost, system: &str) -> serde_json::Value {
    let url = public_url(&host.subdomain, None, system);
    json!({
        "subdomain": host.subdomain,
        "system_domain": system,
        "calendar_url": url,
        "custom_domain": host.custom_domain,
        "custom_domain_status": host.custom_domain_status,
        "custom_domain_checked_at": host.custom_domain_checked_at,
        "custom_domain_last_error": host.custom_domain_last_error,
        "custom_domain_target": format!("{}.{}", host.subdomain, system),
        "public_url": public_url(&host.subdomain, host.custom_domain.as_deref(), system),
        "is_active": host.is_active,
    })
}

/// `GET /api/calendar-host` — this workspace's address, minting it on the first read so a brand-new
/// workspace never has to wait for the sweep.
pub async fn get_host(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let system = system_domain(&s);
    let host = ensure_for_tenant(&s.db, tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound("This workspace no longer exists".into()))?;
    let account_slug: Option<String> = sqlx::query_scalar("SELECT slug FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_optional(&s.db)
        .await?;
    let calendars: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM booking_calendars WHERE tenant_id = $1 AND is_active = true",
    )
    .bind(tenant_id)
    .fetch_one(&s.db)
    .await?;
    let bookings: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM slot_bookings WHERE tenant_id = $1 AND status = 'active'",
    )
    .bind(tenant_id)
    .fetch_one(&s.db)
    .await?;
    let mut payload = host_payload(&host, &system);
    if let Some(map) = payload.as_object_mut() {
        map.insert("active_calendars".into(), json!(calendars));
        map.insert("active_bookings".into(), json!(bookings));
        map.insert("account_slug".into(), json!(account_slug));
    }
    Ok(Json(payload))
}

#[derive(Deserialize)]
pub struct SubdomainRequest {
    pub subdomain: String,
}

/// `PATCH /api/calendar-host/subdomain` — claim a different system subdomain. Uniqueness is the
/// database's call (a 23505 is answered as "already in use"), not a check-then-write race.
pub async fn set_subdomain(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(body): Json<SubdomainRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = require_account_admin(&c)?;
    let system = system_domain(&s);
    let requested = body.subdomain.trim().to_lowercase();
    if requested.is_empty() {
        return Err(AppError::Validation(
            "Type the address you want to use".into(),
        ));
    }
    if !valid_subdomain(&requested) {
        return Err(AppError::Validation(
            "A calendar address uses 3 to 63 letters, numbers and hyphens, and starts and ends with a letter or number. Example: acme-roofing".into(),
        ));
    }
    if is_reserved(&requested) {
        return Err(AppError::Validation(format!(
            "'{requested}' is reserved for the platform. Pick a different name."
        )));
    }
    ensure_for_tenant(&s.db, tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound("This workspace no longer exists".into()))?;

    // A workspace address must not collide with a calendar's own subdomain either (kanban
    // t_d4ac5433) — the wildcard vhost serves both namespaces from the same address space.
    let calendar_owner: Option<Uuid> = sqlx::query_scalar(
        "SELECT cs.calendar_id FROM calendar_subdomains cs WHERE lower(cs.subdomain) = $1",
    )
    .bind(&requested)
    .fetch_optional(&s.db)
    .await?;
    if calendar_owner.is_some() {
        let suggestion: Option<String> = suggest_subdomain(&s.db, &requested)
            .await
            .map(|s| format!("{s}.{system}"));
        return Err(AppError::Conflict(taken_message_with_suggestion(
            &format!("{requested}.{system}"),
            suggestion.as_deref(),
        )));
    }

    let update = sqlx::query(
        "UPDATE calendar_hosts SET subdomain = $1, updated_at = now() WHERE tenant_id = $2",
    )
    .bind(&requested)
    .bind(tenant_id)
    .execute(&s.db)
    .await;

    if let Err(e) = update {
        if is_unique_violation(&e) {
            let suggestion: Option<String> = suggest_subdomain(&s.db, &requested)
                .await
                .map(|s| format!("{s}.{system}"));
            return Err(AppError::Conflict(taken_message_with_suggestion(
                &format!("{requested}.{system}"),
                suggestion.as_deref(),
            )));
        }
        return Err(AppError::Database(e));
    }

    let host = ensure_for_tenant(&s.db, tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound("This workspace no longer exists".into()))?;
    Ok(Json(host_payload(&host, &system)))
}

#[derive(Deserialize)]
pub struct CustomDomainRequest {
    #[serde(alias = "custom_domain", alias = "domain")]
    pub domain: String,
}

/// `PUT /api/calendar-host/custom-domain` — point an owned domain at this calendar. The value starts
/// `pending`: it is not called verified until a request to that address really reaches THIS
/// workspace ([`check_custom_domain`] proves it, and so does the first real visit).
pub async fn set_custom_domain(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(body): Json<CustomDomainRequest>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = require_account_admin(&c)?;
    let system = system_domain(&s);
    let raw = body.domain.trim().to_string();
    if raw.is_empty() {
        return Err(AppError::Validation(
            "Type the domain you want to use".into(),
        ));
    }
    let domain = normalise_domain(&raw)
        .ok_or_else(|| AppError::Validation("That does not look like a domain name".into()))?;
    if !valid_domain(&domain) {
        return Err(AppError::Validation(
            "Use a domain name such as calendar.yourbusiness.com — letters, numbers, dots and hyphens only".into(),
        ));
    }
    if is_system_domain(&domain, &system) {
        return Err(AppError::Validation(format!(
            "Addresses under {system} are already provided. Use your own domain, for example calendar.yourbusiness.com"
        )));
    }
    ensure_for_tenant(&s.db, tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound("This workspace no longer exists".into()))?;
    let update = sqlx::query(
        r#"UPDATE calendar_hosts
           SET custom_domain = $1,
               custom_domain_status = 'pending',
               custom_domain_checked_at = NULL,
               custom_domain_last_error = NULL,
               updated_at = now()
           WHERE tenant_id = $2"#,
    )
    .bind(&domain)
    .bind(tenant_id)
    .execute(&s.db)
    .await;

    if let Err(e) = update {
        if is_unique_violation(&e) {
            // A custom domain has no "-2" suffix to suggest; the honest alternative is the
            // account's own system address, which keeps working no matter what DNS is pointed.
            let current = ensure_for_tenant(&s.db, tenant_id)
                .await?
                .ok_or_else(|| AppError::NotFound("This workspace no longer exists".into()))?;
            let fallback = public_url(&current.subdomain, None, &system);
            return Err(AppError::Conflict(taken_message_with_suggestion(
                &domain,
                Some(&fallback),
            )));
        }
        return Err(AppError::Database(e));
    }
    let host = ensure_for_tenant(&s.db, tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound("This workspace no longer exists".into()))?;
    Ok(Json(host_payload(&host, &system)))
}

/// `DELETE /api/calendar-host/custom-domain` — drop the custom domain. The system subdomain is
/// untouched, so the calendar keeps answering at the address it had before.
pub async fn clear_custom_domain(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = require_account_admin(&c)?;
    let system = system_domain(&s);
    ensure_for_tenant(&s.db, tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound("This workspace no longer exists".into()))?;
    sqlx::query(
        r#"UPDATE calendar_hosts
           SET custom_domain = NULL,
               custom_domain_status = 'none',
               custom_domain_checked_at = NULL,
               custom_domain_last_error = NULL,
               updated_at = now()
           WHERE tenant_id = $1"#,
    )
    .bind(tenant_id)
    .execute(&s.db)
    .await?;
    let host = ensure_for_tenant(&s.db, tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound("This workspace no longer exists".into()))?;
    Ok(Json(host_payload(&host, &system)))
}

/// `POST /api/calendar-host/custom-domain/check` — ask the domain ITSELF. We fetch the address the
/// customer says points here and read which workspace answered. A domain proven to reach this
/// workspace is `verified`; one that answers for someone else, or answers nothing, keeps the honest
/// reason in `custom_domain_last_error` and stays `pending`.
pub async fn check_custom_domain(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    let tenant_id = require_account_admin(&c)?;
    let system = system_domain(&s);
    let host = ensure_for_tenant(&s.db, tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound("This workspace no longer exists".into()))?;
    let Some(domain) = host.custom_domain.clone() else {
        return Err(AppError::Validation(
            "Add a domain first, then check it".into(),
        ));
    };

    let url = format!("https://{domain}/api/public/calendar/site");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .build()
        .map_err(|e| AppError::Internal(format!("Could not start the check: {e}")))?;
    let outcome = client
        .get(&url)
        .header(header::ACCEPT.as_str(), "application/json")
        .send()
        .await;

    let (status, error): (&str, Option<String>) = match outcome {
        Ok(resp) => {
            let code = resp.status();
            if !code.is_success() {
                (
                    "pending",
                    Some(format!(
                        "That address answered with an error ({code}). Point it at {system} first, then check again."
                    )),
                )
            } else {
                match resp.json::<serde_json::Value>().await {
                    Ok(body) => {
                        let answered = body
                            .get("tenant")
                            .and_then(|t| t.get("id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        if answered == tenant_id.to_string() {
                            ("verified", None)
                        } else if answered.is_empty() {
                            (
                                "pending",
                                Some(
                                    "That address answered, but not with a calendar. Point it at this workspace's calendar address, then check again."
                                        .into(),
                                ),
                            )
                        } else {
                            (
                                "pending",
                                Some(
                                    "That address currently shows a different workspace's calendar."
                                        .into(),
                                ),
                            )
                        }
                    }
                    Err(_) => (
                        "pending",
                        Some("That address answered, but not with a calendar.".into()),
                    ),
                }
            }
        }
        Err(e) => {
            let reason = if e.is_timeout() {
                "The check timed out. That usually means the domain does not point here yet."
                    .to_string()
            } else if e.is_connect() {
                "Could not connect to that address. That usually means the DNS record is not in place yet.".to_string()
            } else {
                format!("Could not reach that address: {e}")
            };
            ("pending", Some(reason))
        }
    };

    sqlx::query(
        r#"UPDATE calendar_hosts
           SET custom_domain_status = $1,
               custom_domain_checked_at = now(),
               custom_domain_last_error = $2,
               updated_at = now()
           WHERE tenant_id = $3"#,
    )
    .bind(status)
    .bind(error.as_deref())
    .bind(tenant_id)
    .execute(&s.db)
    .await?;

    let host = ensure_for_tenant(&s.db, tenant_id)
        .await?
        .ok_or_else(|| AppError::NotFound("This workspace no longer exists".into()))?;
    let mut payload = host_payload(&host, &system);
    if let Some(map) = payload.as_object_mut() {
        map.insert("verified".into(), json!(status == "verified"));
    }
    Ok(Json(payload))
}

// ── The hosted page's own data ───────────────────────────────────────────────────────────────────

/// Read the request's `Host` (`x-forwarded-host` first when a proxy set one) and resolve it to a
/// workspace. `None` = this address belongs to no calendar, which the public arms answer as 404.
async fn resolve_request_host(
    s: &AppState,
    headers: &HeaderMap,
) -> Result<Option<super::ResolvedHost>, AppError> {
    let raw = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if raw.is_empty() {
        return Ok(None);
    }
    let system = system_domain(s);
    let resolved = resolve_host(&s.db, &raw, &system).await?;
    Ok(resolved)
}

/// `GET /api/public/calendar/site` — the hosted page's data for the address that was called. The
/// visitor supplies nothing: the workspace (or calendar) comes from the Host. An address that names
/// no calendar is a 404, never somebody else's calendar.
pub async fn public_site(
    State(s): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(public_site_impl(&s, &headers, None).await?))
}

/// `GET /api/public/calendar/site/:slug` — the DEFAULT URL shape
/// `<account-subdomain>.<app-domain>/<calendar-name>` (kanban t_d4ac5433): one specific calendar,
/// named by its slug path segment. The workspace still comes from the Host, never from the URL.
pub async fn public_site_slug(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(SiteSlug { slug }): Path<SiteSlug>,
) -> ApiResult<impl IntoResponse> {
    Ok(Json(public_site_impl(&s, &headers, Some(slug)).await?))
}

#[derive(Deserialize)]
pub struct SiteSlug {
    pub slug: String,
}

/// Shared body of the two public site readers. `requested_slug` is the calendar-name path segment
/// (`/<calendar-name>`), or `None` for the workspace root that lists every active calendar.
async fn public_site_impl(
    s: &AppState,
    headers: &HeaderMap,
    requested_slug: Option<String>,
) -> Result<serde_json::Value, AppError> {
    let system = system_domain(s);
    let raw_host = headers
        .get("x-forwarded-host")
        .or_else(|| headers.get(header::HOST))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    // 1. A dedicated per-calendar subdomain answers for that ONE calendar (kanban t_d4ac5433). The
    //    calendar subdomain is more specific than a workspace subdomain, so it is tried first.
    if let Some(cal) = resolve_calendar_subdomain(&s.db, &raw_host, &system).await? {
        let listings = calendar_listings(s, cal.tenant_id, Some(&cal.calendar_slug)).await?;
        let open_slots = open_slot_count(&listings);
        return Ok(json!({
            "tenant": {
                "id": cal.tenant_id,
                "name": cal.tenant_name,
                "slug": cal.tenant_slug,
                "branding_name": cal.branding_name,
                "logo_url": cal.logo_url,
                "primary_color": cal.primary_color,
                "accent_color": cal.accent_color,
            },
            "host": {
                "kind": "calendar",
                "subdomain": cal.subdomain,
                "system_domain": system,
                "system_url": public_url(&cal.subdomain, None, &system),
                "custom_domain": serde_json::Value::Null,
            },
            "calendars": listings,
            "open_slots": open_slots,
        }));
    }

    // 2. Workspace resolution (system subdomain or an owned custom domain).
    let Some(host) = resolve_request_host(s, headers).await? else {
        return Err(AppError::NotFound("No calendar at this address".into()));
    };

    // A real request arriving on the custom domain IS the proof the record points here, so record
    // it: the console then reads "verified" without the owner having to press Check.
    if host.custom_domain_status != "verified" {
        let asked = host_only(
            headers
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .unwrap_or(""),
        );
        if host.custom_domain.as_deref() == Some(asked.as_str()) {
            let _ = sqlx::query(
                r#"UPDATE calendar_hosts
                   SET custom_domain_status = 'verified', custom_domain_checked_at = now(),
                       custom_domain_last_error = NULL, updated_at = now()
                   WHERE tenant_id = $1 AND lower(custom_domain) = $2"#,
            )
            .bind(host.tenant_id)
            .bind(&asked)
            .execute(&s.db)
            .await;
        }
    }

    let listings = calendar_listings(s, host.tenant_id, requested_slug.as_deref()).await?;
    let open_slots = open_slot_count(&listings);

    Ok(json!({
        "tenant": {
            "id": host.tenant_id,
            "name": host.tenant_name,
            "slug": host.tenant_slug,
            "branding_name": host.branding_name,
            "logo_url": host.logo_url,
            "primary_color": host.primary_color,
            "accent_color": host.accent_color,
        },
        "host": {
            "kind": if host.custom_domain.is_some() && host.custom_domain_status == "verified" { "custom" } else { "system" },
            "subdomain": host.subdomain,
            "system_domain": system,
            "system_url": public_url(&host.subdomain, None, &system),
            "custom_domain": host.custom_domain,
        },
        "calendars": listings,
        "open_slots": open_slots,
    }))
}

/// One calendar's listing (or all of a workspace's), each with its open slot types. `only_slug`
/// narrows the set to a single calendar for the `/site/:slug` arm; a slug that matches nothing
/// answers an empty list, so a bad link shows the branded "nothing open" state, never another
/// calendar.
async fn calendar_listings(
    s: &AppState,
    tenant_id: Uuid,
    only_slug: Option<&str>,
) -> Result<Vec<serde_json::Value>, AppError> {
    let calendars: Vec<CalendarListing> = match only_slug {
        Some(slug) => {
            sqlx::query_as::<_, CalendarListing>(
                r#"SELECT id, slug, name, description, calendar_type
               FROM booking_calendars
               WHERE tenant_id = $1 AND is_active = true AND slug = $2
               ORDER BY name"#,
            )
            .bind(tenant_id)
            .bind(slug)
            .fetch_all(&s.db)
            .await?
        }
        None => {
            sqlx::query_as::<_, CalendarListing>(
                r#"SELECT id, slug, name, description, calendar_type
               FROM booking_calendars
               WHERE tenant_id = $1 AND is_active = true
               ORDER BY name"#,
            )
            .bind(tenant_id)
            .fetch_all(&s.db)
            .await?
        }
    };

    let mut listings = Vec::with_capacity(calendars.len());
    for cal in calendars {
        let slots = sqlx::query_as::<_, SlotListing>(
            r#"SELECT id, slot_name, total_slots, filled_slots, default_duration_days,
                      price_override::text AS price
               FROM calendar_slots
               WHERE calendar_id = $1 AND is_active = true
               ORDER BY sort_order, slot_name"#,
        )
        .bind(cal.id)
        .fetch_all(&s.db)
        .await?;
        listings.push(json!({
            "slug": cal.slug,
            "name": cal.name,
            "description": cal.description,
            "calendar_type": cal.calendar_type,
            "slots": slots.iter().map(|sl| json!({
                "id": sl.id,
                "name": sl.slot_name,
                "total_slots": sl.total_slots,
                "filled_slots": sl.filled_slots,
                "open": if sl.total_slots < 0 { true } else { sl.filled_slots < sl.total_slots },
                "duration_days": sl.default_duration_days,
                "price": sl.price,
            })).collect::<Vec<_>>(),
        }));
    }
    Ok(listings)
}

fn open_slot_count(listings: &[serde_json::Value]) -> usize {
    listings
        .iter()
        .map(|l| {
            l.get("slots")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter(|s| s.get("open") == Some(&json!(true)))
                        .count()
                })
                .unwrap_or(0)
        })
        .sum()
}

#[derive(Deserialize, sqlx::FromRow)]
struct CalendarListing {
    id: Uuid,
    slug: String,
    name: String,
    description: Option<String>,
    calendar_type: String,
}

#[derive(Deserialize, sqlx::FromRow)]
struct SlotListing {
    id: Uuid,
    slot_name: String,
    total_slots: i32,
    filled_slots: i32,
    default_duration_days: i32,
    price: Option<String>,
}

/// `POST /api/public/calendar/book` — book a slot on the calendar at THIS address. The workspace is
/// the one the Host resolved to; the booking write is the same one the rest of the app uses, so a
/// hosted booking carries the same capacity rule, scoring event and round-robin hand-off.
pub async fn public_book(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(mut body): Json<CreateBookingRequest>,
) -> ApiResult<impl IntoResponse> {
    let Some(host) = resolve_request_host(&s, &headers).await? else {
        return Err(AppError::NotFound("No calendar at this address".into()));
    };
    // A caller may repeat the calendar slug and slot; it may NOT name a workspace. Anything the
    // visitor sends is applied to the Host-resolved workspace only.
    body.calendar_slug = body.calendar_slug.trim().to_string();
    if body.calendar_slug.is_empty() {
        return Err(AppError::Validation("Choose a calendar".into()));
    }
    if body.business_name.trim().is_empty() {
        return Err(AppError::Validation(
            "Your business name is required".into(),
        ));
    }
    if !body.contact_email.contains('@') || body.contact_email.len() < 5 {
        return Err(AppError::Validation(
            "A valid email address is required".into(),
        ));
    }
    let (booking, slot_position, _tag) =
        crate::bookings::handlers::create_booking_for_tenant(&s.db, host.tenant_id, body).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "booking_id": booking.id,
            "status": booking.status,
            "slot_position": slot_position,
            "start_date": booking.start_date.to_string(),
            "end_date": booking.end_date.to_string(),
            "workspace": host.tenant_name,
        })),
    ))
}
