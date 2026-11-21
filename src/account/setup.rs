//! "Is this account actually working?" — one honest answer, computed from real rows.
//!
//! WHY THIS EXISTS
//! ---------------
//! A brand-new account opens a product full of EMPTY TABLES. Nothing says which one to fill first,
//! and the things that silently do nothing until they are set up — a sending domain, a booking
//! calendar with a real resource, a tag an automation can fire on — look identical to a broken
//! product. The rule for this fleet is that a workspace with nothing set up must be TOLD what to
//! add, not shown an empty grid and left to guess.
//!
//! This endpoint answers the question by looking at the data, never at a stored "onboarding done"
//! flag: a flag says what somebody clicked, these say what is true. Each step carries the console tab
//! that fixes it, so the panel is a to-do list with doors rather than advice.
//!
//! Adding a step = one query here and one label in the console. Nothing else reads this shape, so it
//! cannot drift from a screen that does not exist yet.

use axum::{extract::State, Json};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::AppError;
use crate::AppState;

/// One setup step: what it is, whether the DATA says it is done, and where to go to do it.
struct Step {
    key: &'static str,
    title: &'static str,
    why: &'static str,
    tab: &'static str,
    done: bool,
    /// What was actually found, in the customer's language — never "true"/"false".
    detail: String,
}

impl Step {
    fn json(&self) -> Value {
        json!({
            "key": self.key,
            "title": self.title,
            "why": self.why,
            "tab": self.tab,
            "done": self.done,
            "detail": self.detail,
        })
    }
}

/// Counts and booleans, in one round trip each, all scoped to the caller's account.
async fn scalar_i64(db: &PgPool, sql: &str, tid: Uuid) -> i64 {
    sqlx::query_scalar::<_, i64>(sql)
        .bind(tid)
        .fetch_one(db)
        .await
        .unwrap_or(0)
}

pub async fn account_setup(
    State(state): State<AppState>,
    axum::Extension(c): axum::Extension<Claims>,
) -> Result<Json<Value>, AppError> {
    let tid = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let db = &state.db;

    // 1. A verified sending domain. Without one the account has no identity to send from, so every
    //    automation, campaign and receipt has to borrow the platform's.
    let verified_domains = scalar_i64(
        db,
        "SELECT count(*) FROM private_email_domains WHERE tenant_id = $1 AND verified = true",
        tid,
    )
    .await;
    let any_domain = scalar_i64(
        db,
        "SELECT count(*) FROM private_email_domains WHERE tenant_id = $1",
        tid,
    )
    .await;

    // 2. A mailbox to receive on. A domain with no box is a domain that collects nothing.
    let boxes = scalar_i64(
        db,
        "SELECT count(*) FROM private_email_boxes WHERE tenant_id = $1",
        tid,
    )
    .await;

    // 3. A booking calendar that can actually offer times: the calendar alone offers none until it
    //    has a resource, which is the trap measured on a real account.
    let calendars = scalar_i64(
        db,
        "SELECT count(*) FROM booking_calendars WHERE tenant_id = $1 AND is_active = true",
        tid,
    )
    .await;
    let bookable = scalar_i64(
        db,
        "SELECT count(*) FROM booking_resources r \
           JOIN booking_calendars c ON c.id = r.calendar_id \
          WHERE c.tenant_id = $1 AND c.is_active = true AND r.active = true",
        tid,
    )
    .await;

    // 4. A tag — the thing automations fire on. Every rule in this product is triggered by one.
    let tags = scalar_i64(
        db,
        "SELECT count(*) FROM tags WHERE tenant_id = $1 AND is_active = true",
        tid,
    )
    .await;

    // 5. A live automation rule.
    let rules = scalar_i64(
        db,
        "SELECT count(*) FROM automation_rules WHERE tenant_id = $1 AND is_active = true",
        tid,
    )
    .await;

    // 6. A pipeline with stages — deals need somewhere to sit.
    let staged_pipelines = scalar_i64(
        db,
        "SELECT count(DISTINCT p.id) FROM pipelines p \
           JOIN pipeline_stages s ON s.pipeline_id = p.id \
          WHERE p.tenant_id = $1",
        tid,
    )
    .await;

    // 7. At least one contact, so nothing else is being configured in a vacuum.
    let contacts = scalar_i64(
        db,
        "SELECT count(*) FROM contacts WHERE tenant_id = $1",
        tid,
    )
    .await;

    // 8. An active API key. Every account mints one at signup, so a missing key here means somebody
    //    revoked it — and an AI agent then cannot reach the account at all.
    let keys = scalar_i64(
        db,
        "SELECT count(*) FROM personal_api_keys WHERE tenant_id = $1 AND is_active = true",
        tid,
    )
    .await;

    // 9. A teammate, which is what makes assignment mean anything.
    let people = scalar_i64(
        db,
        "SELECT count(*) FROM users WHERE tenant_id = $1 AND is_active = true",
        tid,
    )
    .await;

    let steps = vec![
        Step {
            key: "sending_domain",
            title: "Add and verify a sending domain",
            why: "Until a domain is verified, everything this account sends has to borrow the platform's \
                  address, and the platform's reputation.",
            tab: "email-settings",
            done: verified_domains > 0,
            detail: if verified_domains > 0 {
                format!("{verified_domains} domain(s) verified.")
            } else if any_domain > 0 {
                "A domain is added but not verified yet — add the DNS records it lists.".to_string()
            } else {
                "No domain yet.".to_string()
            },
        },
        Step {
            key: "inbox_address",
            title: "Create a mailbox to receive on",
            why: "A domain with no mailbox collects nothing, and a reply has no address to come from.",
            tab: "inbox",
            done: boxes > 0,
            detail: if boxes > 0 {
                format!("{boxes} mailbox(es) ready.")
            } else {
                "No mailbox yet.".to_string()
            },
        },
        Step {
            key: "booking",
            title: "Make a calendar that can be booked",
            why: "A calendar offers no times until it has a resource, so a shared booking link would \
                  show an empty page.",
            tab: "booking",
            done: bookable > 0,
            detail: if bookable > 0 {
                format!("{bookable} bookable resource(s) across {calendars} calendar(s).")
            } else if calendars > 0 {
                "A calendar exists but has nothing to book — add a resource.".to_string()
            } else {
                "No calendar yet.".to_string()
            },
        },
        Step {
            key: "tags",
            title: "Create your first tag",
            why: "Tags are what automations fire on. Without one, a rule has nothing to start it.",
            tab: "tags",
            done: tags > 0,
            detail: if tags > 0 {
                format!("{tags} tag(s).")
            } else {
                "No tags yet.".to_string()
            },
        },
        Step {
            key: "automation",
            title: "Switch on an automation rule",
            why: "This is the part that does the work while nobody is watching — a welcome mail, a \
                  follow-up, a nudge when a deal goes quiet.",
            tab: "rules",
            done: rules > 0,
            detail: if rules > 0 {
                format!("{rules} live rule(s).")
            } else {
                "No live rules yet.".to_string()
            },
        },
        Step {
            key: "pipeline",
            title: "Set up a pipeline with stages",
            why: "Deals need somewhere to sit, and a board with no stages has nowhere to drag them.",
            tab: "pipelines",
            done: staged_pipelines > 0,
            detail: if staged_pipelines > 0 {
                format!("{staged_pipelines} pipeline(s) with stages.")
            } else {
                "No staged pipeline yet.".to_string()
            },
        },
        Step {
            key: "contacts",
            title: "Add your first contact",
            why: "Everything else in the product describes people — deals, tags, bookings and \
                  conversations all hang off a contact.",
            tab: "contacts",
            done: contacts > 0,
            detail: if contacts > 0 {
                format!("{contacts} contact(s).")
            } else {
                "No contacts yet.".to_string()
            },
        },
        Step {
            key: "api_key",
            title: "Keep an API key active",
            why: "The key is how an AI agent or your own scripts reach this account. Every account is \
                  issued one at signup.",
            tab: "mcp",
            done: keys > 0,
            detail: if keys > 0 {
                format!("{keys} active key(s).")
            } else {
                "No active key — mint one on the MCP screen.".to_string()
            },
        },
        Step {
            key: "team",
            title: "Invite a teammate",
            why: "One person cannot answer every conversation. Assignment only means something once \
                  there is somebody to assign to.",
            tab: "team",
            done: people > 1,
            detail: if people > 1 {
                format!("{people} active people.")
            } else {
                "Just you so far.".to_string()
            },
        },
    ];

    let done = steps.iter().filter(|s| s.done).count();
    let total = steps.len();
    // The NEXT thing to do, named. A panel that lists nine items and does not say which one first is
    // a wall, not a guide.
    let next = steps
        .iter()
        .find(|s| !s.done)
        .map(|s| json!({ "key": s.key, "title": s.title, "tab": s.tab }));

    Ok(Json(json!({
        "account_id": tid,
        "done": done,
        "total": total,
        "all_done": done == total,
        "next": next,
        "steps": steps.iter().map(|s| s.json()).collect::<Vec<_>>(),
    })))
}
