//! What a booking DOES to the CRM: several tags and several lists, per calendar.
//!
//! A purchase is not one fact. It is the product bought, the city the buyer came from, and how warm the
//! lead is — three tags on one contact — and that contact then belongs on more than one list, because a
//! nurture list and a customer list are different lists for the same person. The old shape was a single
//! `booking_calendars.sponsor_tag` column, which could express exactly one of those and could not put
//! anybody on a list at all.
//!
//! Tags are TEMPLATES: `{product}`, `{city}`, `{calendar}` are substituted from what the buyer actually
//! chose. A template whose placeholder resolves to nothing is SKIPPED rather than applied half-empty —
//! a tag called " - Sponsor" is worse than no tag, because nothing downstream can match it.
//!
//! The booking is never lost to this work: every failure is a warning, not an error. A sponsor who paid
//! must not have their order rolled back because a tag name was rejected.

use sqlx::PgPool;
use uuid::Uuid;

use crate::booking::models::Calendar;

/// What the buyer chose, as far as the tags are concerned.
#[derive(Debug, Clone, Default)]
pub struct CaptureCtx {
    pub calendar: String,
    pub product: String,
    pub city: Option<String>,
}

impl CaptureCtx {
    /// Substitute the placeholders, or `None` when one of them has nothing to say.
    pub fn resolve(&self, template: &str) -> Option<String> {
        let mut out = template.to_string();
        if out.contains("{product}") {
            if self.product.trim().is_empty() {
                return None;
            }
            out = out.replace("{product}", self.product.trim());
        }
        if out.contains("{city}") {
            let city = self.city.as_deref()?.trim();
            if city.is_empty() {
                return None;
            }
            out = out.replace("{city}", city);
        }
        if out.contains("{calendar}") {
            if self.calendar.trim().is_empty() {
                return None;
            }
            out = out.replace("{calendar}", self.calendar.trim());
        }
        let out = out.trim().to_string();
        if out.is_empty() || out.contains('{') {
            // An unknown placeholder is a configuration mistake: refuse to invent a tag name from it.
            return None;
        }
        Some(out)
    }
}

/// The tags this calendar puts on a booking's contact, in the order they were added.
pub async fn tag_templates(pool: &PgPool, calendar_id: Uuid) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT template FROM booking_calendar_capture_tags
          WHERE calendar_id = $1 ORDER BY created_at, template",
    )
    .bind(calendar_id)
    .fetch_all(pool)
    .await
}

/// The lists this calendar adds a booking's contact to.
pub async fn list_ids(pool: &PgPool, calendar_id: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT list_id FROM booking_calendar_capture_lists
          WHERE calendar_id = $1 ORDER BY created_at",
    )
    .bind(calendar_id)
    .fetch_all(pool)
    .await
}

/// The tag CATEGORIES this calendar applies in full.
pub async fn category_ids(pool: &PgPool, calendar_id: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT category_id FROM booking_calendar_capture_categories
          WHERE calendar_id = $1 ORDER BY created_at",
    )
    .bind(calendar_id)
    .fetch_all(pool)
    .await
}

/// Every tag name inside a category, oldest first.
pub async fn tags_in_category(
    pool: &PgPool,
    tenant: Uuid,
    category_id: Uuid,
) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT name FROM tags WHERE tenant_id = $1 AND category_id = $2 ORDER BY created_at, name",
    )
    .bind(tenant)
    .bind(category_id)
    .fetch_all(pool)
    .await
}

/// Apply every tag and add the contact to every list this calendar carries. Never returns an error:
/// the booking has already been committed by the time this runs.
pub async fn apply_all(pool: &PgPool, cal: &Calendar, contact_id: Uuid, ctx: &CaptureCtx) {
    let mut applied: Vec<String> = Vec::new();

    match tag_templates(pool, cal.id).await {
        Ok(templates) => {
            for t in templates {
                let Some(name) = ctx.resolve(&t) else {
                    tracing::warn!(
                        calendar = %cal.id, template = %t,
                        "a capture tag is skipped: its placeholder has no value on this booking"
                    );
                    continue;
                };
                if applied.iter().any(|a| a.eq_ignore_ascii_case(&name)) {
                    continue;
                }
                match crate::tags::apply_by_name(
                    pool,
                    cal.tenant_id,
                    "contact",
                    contact_id,
                    &name,
                    None,
                )
                .await
                {
                    Ok(_) => applied.push(name),
                    Err(e) => tracing::warn!(error = %e, tag = %name,
                        "the booking stands but one of its tags could not be applied"),
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, calendar = %cal.id,
            "the booking stands but its tag templates could not be read"),
    }

    // CATEGORIES: every tag the owner has already grouped, applied whole. Reading the members at apply
    // time is the point — a product added to the category tomorrow is on tomorrow's bookings without
    // anybody editing ten calendars.
    match category_ids(pool, cal.id).await {
        Ok(ids) => {
            for category_id in ids {
                match tags_in_category(pool, cal.tenant_id, category_id).await {
                    Ok(names) => {
                        for name in names {
                            if applied.iter().any(|a| a.eq_ignore_ascii_case(&name)) {
                                continue;
                            }
                            match crate::tags::apply_by_name(
                                pool,
                                cal.tenant_id,
                                "contact",
                                contact_id,
                                &name,
                                None,
                            )
                            .await
                            {
                                Ok(_) => applied.push(name),
                                Err(e) => tracing::warn!(error = %e, tag = %name,
                                    "the booking stands but a category tag could not be applied"),
                            }
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, category = %category_id,
                        "the booking stands but one category's tags could not be read"),
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, calendar = %cal.id,
            "the booking stands but its tag categories could not be read"),
    }

    match list_ids(pool, cal.id).await {
        Ok(ids) => {
            for list_id in ids {
                // ON CONFLICT DO NOTHING: a returning sponsor who is already on the list is not an error
                // and must not fire a second ListAdded (that would re-run their welcome automation).
                let inserted = sqlx::query(
                    "INSERT INTO list_members (id, list_id, contact_id, tenant_id, added_manually)
                     VALUES ($1, $2, $3, $4, false)
                     ON CONFLICT DO NOTHING",
                )
                .bind(Uuid::new_v4())
                .bind(list_id)
                .bind(contact_id)
                .bind(cal.tenant_id)
                .execute(pool)
                .await;
                match inserted {
                    Ok(r) if r.rows_affected() > 0 => {
                        crate::automation::engine::fire_list_trigger(
                            pool,
                            cal.tenant_id,
                            contact_id,
                            list_id,
                            "ListAdded",
                        )
                        .await;
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, list = %list_id,
                        "the booking stands but the contact could not be added to one of its lists"),
                }
            }
        }
        Err(e) => tracing::warn!(error = %e, calendar = %cal.id,
            "the booking stands but its lists could not be read"),
    }
}
