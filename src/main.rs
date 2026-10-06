#![allow(clippy::all)]
//! CRM Swift — Multi-account Lead Management Operating System
//!
//! Each "account" is backed by a DB tenant. "Team members" are users under an account.
//! This is the main entry point for the Axum-based REST API server.
//! The server provides a fully-featured CRM with contacts, pipelines,
//! lead scoring, automation, and integration capabilities.

pub mod account;
pub mod admin_actions;
pub mod ai;
pub mod analytics;
pub mod audit;
pub mod auth;
pub mod automation;
pub mod billing;
mod body_deadline;
pub mod bookings;
pub mod campaigns;
pub mod checklists;
pub mod communications;
pub mod companies;
mod config;
pub mod contacts;
pub mod contacts_internal;
pub mod csv_handler;
pub mod dashboard;
mod db;
pub mod email;
pub mod email_templates;
mod errors;
pub mod events;
pub mod external_api;
mod features;
pub mod google_calendar;
pub mod inbound;
pub mod industries;
pub mod integration_center;
pub mod integrations;
pub mod lists;
pub mod lists_internal;
pub mod messages;
pub mod module_registry;
pub mod monitoring;
pub mod native_apps;
pub mod notifications;
pub mod personal_api_keys;
pub mod pipelines;
pub mod plans;
pub mod portfolio;
pub mod private_email;
pub mod profile;
pub mod provider_keys;
pub mod rate_limiter;
pub mod round_robin;
pub mod scoring;
pub mod secret_box;
pub mod security;
pub mod sql_json;
pub mod support_widgets;
pub mod tag_provision_handler;
pub mod tags;
pub mod telnyx;
pub mod tenants_internal;
pub mod tickets;
pub mod tracked_links;
pub mod webhook;
pub mod webhooks;
pub mod worker;

use axum::{
    http::{HeaderValue, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use std::time::Duration;
use tokio::signal;
use tower_http::{
    compression::CompressionLayer, cors::CorsLayer, services::ServeDir, trace::TraceLayer,
};
use tracing_subscriber::EnvFilter;

/// Application state shared across all handlers
#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub redis: redis::aio::ConnectionManager,
    pub config: config::AppConfig,
    /// Built ONCE at startup and shared. Measured 2026-10-02: `RateLimiterState` was defined and its
    /// middlewares written, but NOTHING ever constructed it and it was not on the state — so the
    /// middlewares could not have been mounted even if someone had tried. That is why the rate limiter
    /// was decorative.
    pub rate_limiter: crate::rate_limiter::RateLimiterState,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("config", &self.config)
            .finish()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize jsonwebtoken crypto provider (required by jsonwebtoken v10)
    jsonwebtoken::crypto::CryptoProvider::install_default(
        &jsonwebtoken::crypto::aws_lc::DEFAULT_PROVIDER,
    )
    .ok();

    // Initialize tracing with structured logging
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(true)
        .with_thread_ids(true)
        .init();

    // ── Host-side utility mode ────────────────────────────────────────────────────────────────
    // The static marketing page + legal pages live on the HOST (/opt/swift/nginx/www/coreswift/)
    // and the server runs in a container with ZERO mounts for them, so the request path never
    // writes them (that attempt is what made PUT /api/admin/site answer 500 after its row had
    // already committed). Running the SAME binary on the host with this argument is what
    // materializes them; /opt/swift/bin/cs-site-apply.sh drives it from cron.
    //
    // Checked BEFORE the config/Redis/listener come up: it needs DATABASE_URL and nothing else, and
    // it must never start a second API against the live port.
    //
    //   crm-swift apply-site-settings            write only the files whose bytes would change
    //   crm-swift apply-site-settings --check    render and report, write NOTHING
    //   crm-swift apply-site-settings --emit DIR also write the rendered bytes under DIR (a
    //                                            read-only-in-SITE_ROOT comparison artifact)
    if std::env::args().nth(1).as_deref() == Some("apply-site-settings") {
        apply_site_settings_mode().await;
        return Ok(());
    }

    // Load environment variables
    let config = config::AppConfig::from_env()?;

    // Connect to PostgreSQL with connection pooling
    let db = db::connect(
        &config.database_url,
        config.db_min_connections,
        config.db_max_connections,
    )
    .await?;

    // Run database migrations. A failure here is NOT benign and must never be reported as one.
    //
    // `sqlx::migrate!` resolves ONE file per version and compares its checksum with the applied row,
    // so a single duplicate or edited version aborts the WHOLE run, and `run_direct` validates the
    // applied set BEFORE applying anything — meaning a `VersionMismatch`/`VersionMissing` leaves this
    // process serving a schema that is silently behind the code it was built from. Measured on this
    // host 2026-09-22 (card t_10dd6fc7): a duplicate version number (`80_` vs `080_`) aborted every
    // boot for hours, no pending migration was ever applied, migration 082 had to be applied to live
    // by hand, and the only signal was a WARN that read like a benign no-op.
    //
    // So: ERROR, naming the version in prose via `%e` ("migration 80 was previously applied but has
    // been modified") and the sqlx error VARIANT via `error_kind` (`VersionMismatch`,
    // `VersionMissing`, `Dirty`, `ExecuteMigration`, …), then refuse to serve — a schema that does
    // not match this binary fails the deploy instead of being served. The health signal for that
    // state is the process itself: the container crash-loops and the docker healthcheck / fleet
    // watchdog trip. `MIGRATIONS_FATAL=0` is the single documented escape hatch (boot anyway, still
    // ERROR, and the health payload reports it). There is deliberately no silent mode.
    match sqlx::migrate!("./migrations").run(&db).await {
        Ok(_) => tracing::info!("Database migrations completed successfully"),
        Err(e) => {
            let kind = migration_error_kind(&e);
            tracing::error!(
                error = %e,
                error_kind = %kind,
                "MIGRATION RUN FAILED — no further pending migration was applied, so the schema may be behind this binary"
            );
            if migrations_fatal() {
                tracing::error!(
                    "Refusing to serve: fix the migration (never edit an applied file — add a new version instead, see docs/fleet-migration-guard-convention-2026-09-21.md) and redeploy, or set MIGRATIONS_FATAL=0 to boot anyway with the schema behind the code."
                );
                std::process::exit(1);
            }
            // Keep the text for the health payload: this process is about to serve with migrations
            // UNAPPLIED, and `status: "ok"` on its own would hide that.
            let _ = MIGRATION_FAILURE.set(e.to_string());
            tracing::error!(
                "MIGRATIONS_FATAL=0: booting anyway with migrations UNAPPLIED — the schema may be behind this binary"
            );
        }
    }

    // Seal any provider key still stored in plaintext (CS-21). Idempotent and non-fatal: every read
    // path passes through `secret_box::open`, which understands both forms, so a failure here cannot
    // take the app down or lose a credential.
    match crate::secret_box::backfill_provider_keys(&db).await {
        Ok(n) => tracing::info!(sealed = n, "provider keys at rest are encrypted"),
        Err(e) => tracing::warn!(error = %e, "provider key backfill skipped"),
    }

    // The storage guard (`migrations/075_…`) is armed NOT VALID so it could be added to a table that
    // still held legacy rows; now that the backfill has sealed every row there is nothing to exempt,
    // so validating it makes the constraint fully enforced. Best-effort: a missing constraint warns.
    match crate::secret_box::validate_provider_key_guard(&db).await {
        Ok(true) => tracing::info!("provider key storage guard validated (every row sealed)"),
        Ok(false) => tracing::warn!("provider key storage guard stays NOT VALID"),
        Err(e) => tracing::warn!(error = %e, "provider key storage guard not validated"),
    }

    // t_6718dc86: the same posture for the outbound webhook signing secret (`migrations/078`). The
    // table held no rows when the guard was armed, so nothing is exempt and the constraint can be
    // fully enforced; a plaintext row still in there keeps it NOT VALID and says so at every boot.
    match crate::secret_box::validate_webhook_guard(&db).await {
        Ok(true) => {
            tracing::info!("webhook endpoint storage guard validated (every secret sealed)")
        }
        Ok(false) => tracing::warn!("webhook endpoint storage guard stays NOT VALID"),
        Err(e) => tracing::warn!(error = %e, "webhook endpoint storage guard not validated"),
    }

    // t_706da9df: the Google OAuth refresh token on a booking calendar is a STANDING grant on the
    // tenant's Google account (it survives key rotation, and the tenant cannot revoke it from
    // CoreSwift), so it gets both halves every other credential has: a backfill for rows written
    // before the fix, and the boot validate that makes `migrations/079`'s guard enforced instead of
    // an armed-but-unasserted NOT VALID constraint.
    match crate::secret_box::backfill_booking_calendar_tokens(&db).await {
        Ok(n) => tracing::info!(sealed = n, "booking calendar grant backfill complete"),
        Err(e) => tracing::warn!(error = %e, "booking calendar grant backfill skipped"),
    }
    match crate::secret_box::validate_booking_calendar_guard(&db).await {
        Ok(true) => {
            tracing::info!("booking calendar storage guard validated (every grant sealed)")
        }
        Ok(false) => tracing::warn!("booking calendar storage guard stays NOT VALID"),
        Err(e) => tracing::warn!(error = %e, "booking calendar storage guard not validated"),
    }

    // t_477d46c2: `integration_targets.api_key` is the outbound credential a tenant pastes for a
    // webhook/n8n/Zapier target. It is sealed on create; these are the two halves every other
    // credential column has and this one was missing — a backfill so a row written before the fix is
    // sealed rather than left in the clear beside sealed ones, and the boot validate that turns
    // `migrations/077`'s armed-`NOT VALID` constraint into a fully enforced one.
    match crate::secret_box::backfill_integration_target_keys(&db).await {
        Ok(n) => tracing::info!(
            sealed = n,
            "integration target credential backfill complete"
        ),
        Err(e) => tracing::warn!(error = %e, "integration target credential backfill skipped"),
    }
    match crate::secret_box::validate_integration_target_guard(&db).await {
        Ok(true) => {
            tracing::info!("integration target storage guard validated (every credential sealed)")
        }
        Ok(false) => tracing::warn!("integration target storage guard stays NOT VALID"),
        Err(e) => tracing::warn!(error = %e, "integration target storage guard not validated"),
    }

    // Seal-on-WRITE assertion (CS-21b). The backfill above repairs history; this makes sure history
    // cannot quietly repeat — a column that holds a plaintext secret is reported on EVERY boot
    // instead of being tolerated forever. `CORESWIFT_SECRET_AUDIT=1` turns it into a one-shot check
    // that prints the findings and exits non-zero, so the live container can be audited without a
    // restart.
    let audit_requested = std::env::var("CORESWIFT_SECRET_AUDIT").is_ok();
    match crate::secret_box::audit_plaintext_secrets(&db).await {
        Ok(audit) => {
            // A credential the app cannot open is not a leak, but it IS broken (rotated
            // CORESWIFT_SECRET, or a row written by another deployment) — warn, don't cry wolf.
            for f in &audit.unreadable {
                tracing::warn!(
                    table = f.table,
                    column = f.column,
                    row = %f.row_id,
                    reason = f.reason,
                    "stored secret cannot be opened"
                );
            }
            if audit.plaintext.is_empty() {
                tracing::info!(
                    unreadable = audit.unreadable.len(),
                    "secret audit: 0 plaintext secrets at rest"
                );
                if audit_requested {
                    println!(
                        "{}",
                        serde_json::json!({ "status": "CLEAN", "plaintext_secrets": [],
                                            "unreadable": audit.unreadable })
                    );
                    std::process::exit(0);
                }
            } else {
                tracing::error!(
                    count = audit.plaintext.len(),
                    "secret audit: PLAINTEXT SECRETS AT REST — a write path is not sealing"
                );
                for f in &audit.plaintext {
                    tracing::error!(
                        table = f.table,
                        column = f.column,
                        row = %f.row_id,
                        reason = f.reason,
                        "plaintext secret"
                    );
                }
                if audit_requested {
                    println!(
                        "{}",
                        serde_json::json!({ "status": "DIRTY", "plaintext_secrets": audit.plaintext,
                                            "unreadable": audit.unreadable })
                    );
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "secret audit skipped");
            if audit_requested {
                println!(
                    "{}",
                    serde_json::json!({ "status": "ERROR", "error": e.to_string() })
                );
                std::process::exit(2);
            }
        }
    }

    // Connect to Redis
    let redis = db::connect_redis(&config.redis_url).await?;
    tracing::info!("Connected to Redis");

    // Build shared state
    let state = AppState {
        db,
        redis,
        rate_limiter: crate::rate_limiter::RateLimiterState::from_config(&config),
        config: config.clone(),
    };

    // Request ID middleware — adds X-Request-Id to every response
    let request_id_middleware = axum::middleware::from_fn(request_id_middleware_fn);

    // Body-read deadline (kanban t_59745689): how long a request body may take to ARRIVE before the
    // request is answered 408 and its task, connection and partial body buffer are released. In the
    // boot log for the same reason the migration posture above is — the bound an operator relies on
    // has to be visible without reading the source.
    tracing::info!(
        "Request body-read deadline: {:?} on every route that reads a body, 408 above that (BODY_READ_DEADLINE_SECS)",
        body_deadline::BodyReadDeadline::from_secs(config.body_read_deadline_secs).duration()
    );
    let body_read_deadline =
        body_deadline::BodyReadDeadline::from_secs(config.body_read_deadline_secs);

    // Telnyx webhook verification posture (kanban t_fd5000e1), in the boot log for the same reason
    // the body deadline is above: whether the two public Telnyx receivers can accept ANYTHING has to
    // be readable without opening the source. Unset key = every delivery refused, which an operator
    // needs to see before wondering why inbound SMS stopped.
    tracing::info!(
        "Telnyx webhook verification: {} (telnyx-signature-ed25519 over `<telnyx-timestamp>|<body>`, {}s tolerance, TELNYX_PUBLIC_KEY)",
        match config.telnyx_public_key.as_deref() {
            Some(_) => "ENABLED on /api/telnyx/webhook and /api/telnyx/sms-webhook".to_string(),
            None => "NOT CONFIGURED — both receivers refuse every delivery with 503 until TELNYX_PUBLIC_KEY is set".to_string(),
        },
        config.telnyx_signature_tolerance_secs
    );

    // Rate-limit posture in the boot log for the same reason the body deadline is above: the
    // bounds an operator relies on have to be readable without opening the source (t_3130f105).
    tracing::info!(
        "API rate limits (per IP): {} /min anonymous, {} /min with a bearer credential, {} /min on auth routes, {} per {} min on password recovery",
        config.api_rate_limit_per_minute,
        config.console_rate_limit_per_minute,
        config.auth_rate_limit_per_minute,
        config.password_rate_limit_max,
        config.password_rate_limit_window_minutes
    );

    // Build the complete router
    let app = Router::new()
        // Health check (no auth required)
        .route("/api/health", get(health_check))
        .route("/api/ready", get(ready_check))
        // Serve SPA at root
        .nest_service("/", ServeDir::new("public"))
        // Auth routes (public group + auth-middleware-guarded /invite & /me/usage)
        .nest("/api/auth", auth::router(state.clone()))
        // Protected routes
        .nest("/api/account", account::router(state.clone()))
        // The signed-in user's OWN record — name + password. The workspace Profile tab has always
        // called these two paths; until now neither was registered (both answered 405).
        .nest("/api/profile", profile::router(state.clone()))
        .nest("/api/contacts", contacts::router(state.clone()))
        // CSV import/export — /api/csv/{preview,import/contacts,export/contacts,export/opportunities}
        .nest("/api/csv", csv_handler::router(state.clone()))
        .nest(
            "/api/internal/contacts",
            contacts_internal::router().layer(axum::middleware::from_fn_with_state(
                body_read_deadline,
                body_deadline::body_read_deadline_middleware,
            )),
        )
        // FunnelSwift tag provision webhook — auto-provision free-tier contacts
        .route(
            "/api/v1/internal/tag-provision",
            axum::routing::post(tag_provision_handler::handle_tag_provision).layer(
                axum::middleware::from_fn_with_state(
                    body_read_deadline,
                    body_deadline::body_read_deadline_middleware,
                ),
            ),
        )
        // The ACCOUNT door beside it (kanban t_e968e9ad): the same caller asks this app to mint
        // the free account — workspace + entry plan + owner user + credentials mail — for a lead
        // tagged with `CoreSwift — Free`. Mounted here (outside every module's own auth layer) for
        // the same reason as its sibling; the shared key is demanded by the credential boundary
        // (it is named in `auth::route_policy::INTERNAL_ROUTES`) and again inside the handler.
        .route(
            "/api/v1/internal/provision-free-account",
            axum::routing::post(tag_provision_handler::handle_provision_free_account).layer(
                axum::middleware::from_fn_with_state(
                    body_read_deadline,
                    body_deadline::body_read_deadline_middleware,
                ),
            ),
        )
        // Unified Inbox — messages webhook from MD/IS. Fire-and-forget on the SENDER's side, but not
        // anonymous here: it WRITES a `cs_messages` row, so it demands the shared `x-internal-key`
        // at the credential boundary (named in `auth::route_policy::INTERNAL_ROUTES`) and again in
        // the handler. Mounted outside every module's own auth layer for the same reason as the two
        // internal doors above (kanban t_36cf12d0).
        .route(
            "/api/messages/webhook",
            axum::routing::post(messages::handlers::webhook_receive).layer(
                axum::middleware::from_fn_with_state(
                    body_read_deadline,
                    body_deadline::body_read_deadline_middleware,
                ),
            ),
        )
        .nest("/api/companies", companies::router(state.clone()))
        .nest("/api/pipelines", pipelines::router(state.clone()))
        .nest("/api/tags", tags::router(state.clone()))
        .nest("/api/scoring", scoring::router(state.clone()))
        .nest("/api/lists", lists::router(state.clone()))
        .nest(
            "/api/internal/lists",
            lists_internal::router().layer(axum::middleware::from_fn_with_state(
                body_read_deadline,
                body_deadline::body_read_deadline_middleware,
            )),
        )
        // Unified Inbox — protected CRUD for messages
        .nest("/api/messages", messages::router(state.clone()))
        .nest(
            "/api/internal/tenants",
            tenants_internal::router().layer(axum::middleware::from_fn_with_state(
                body_read_deadline,
                body_deadline::body_read_deadline_middleware,
            )),
        )
        .nest(
            "/api/internal/tags",
            tags::internal_handler::router().layer(axum::middleware::from_fn_with_state(
                body_read_deadline,
                body_deadline::body_read_deadline_middleware,
            )),
        )
        .nest("/api/integrations", integrations::router(state.clone()))
        // Hub Integration Center — what feeds this CRM (lead sources) + what it feeds
        .nest(
            "/api/integration-center",
            integration_center::router(state.clone()),
        )
        .nest("/api", provider_keys::router(state.clone()))
        .nest(
            "/api/external",
            external_api::router().layer(axum::middleware::from_fn_with_state(
                body_read_deadline,
                body_deadline::body_read_deadline_middleware,
            )),
        )
        .nest(
            "/api/personal-api-keys",
            personal_api_keys::router(state.clone()),
        )
        .nest("/api/analytics", analytics::router(state.clone()))
        .nest("/api/ai", ai::router(state.clone()))
        // Billing (plan tiers, feature toggles)
        .nest("/api/campaigns", campaigns::router(state.clone()))
        .nest("/api/billing", billing::router(state.clone()))
        // Audit logs (system-wide event trail)
        .nest("/api/audit", audit::router(state.clone()))
        // Event webhook hub
        .nest("/api/events", events::router(state.clone()))
        // Communications (Twilio/SendGrid orchestration)
        .nest("/api/comms", communications::router(state.clone()))
        // Native app connectors (AdaSwift, FunnelSwift, WorkflowSwift, etc.)
        .nest("/api/native", native_apps::router(state.clone()))
        // Public webhook — single endpoint for n8n and Hermes
        .nest(
            "/api/webhook",
            webhook::router().layer(axum::middleware::from_fn_with_state(
                body_read_deadline,
                body_deadline::body_read_deadline_middleware,
            )),
        )
        // Dashboard — aggregate stats
        .nest("/api/dashboard", dashboard::router(state.clone()))
        // Portfolio — multi-entity portfolio companies
        .nest("/api/portfolio", portfolio::router(state.clone()))
        .nest("/api/bookings", bookings::router(state.clone()))
        .nest(
            "/api/bookings/internal",
            bookings::internal_router().layer(axum::middleware::from_fn_with_state(
                body_read_deadline,
                body_deadline::body_read_deadline_middleware,
            )),
        )
        // Alternative internal calendar creation path (outside auth middleware)
        .nest(
            "/api/internal/bookings",
            bookings::internal_router().layer(axum::middleware::from_fn_with_state(
                body_read_deadline,
                body_deadline::body_read_deadline_middleware,
            )),
        )
        // Round-robin lead assignment
        .nest("/api/round-robin", round_robin::router(state.clone()))
        // Inbound webhook — receive events from satellite apps
        .nest(
            "/inbound",
            inbound::router().layer(axum::middleware::from_fn_with_state(
                body_read_deadline,
                body_deadline::body_read_deadline_middleware,
            )),
        )
        // Admin chat actions — run the entire business from Telegram
        .nest("/api/admin", admin_actions::router(state.clone()))
        // Onboarding checklists
        .nest("/api/checklists", checklists::router(state.clone()))
        // Account health monitoring
        .nest("/api/monitoring", monitoring::router(state.clone()))
        // In-app notifications
        .nest("/api/notifications", notifications::router(state.clone()))
        // Telnyx SMS/Voice integration
        .nest("/api/telnyx", telnyx::router(state.clone()))
        // Cross-app webhooks — receive tag sync events from satellite apps
        .nest(
            "/api/v1/webhooks",
            webhooks::cross_app_tag_sync::router().layer(axum::middleware::from_fn_with_state(
                body_read_deadline,
                body_deadline::body_read_deadline_middleware,
            )),
        )
        // Google Calendar sync — OAuth2, push/pull events
        .nest(
            "/api/google-calendar",
            google_calendar::router(state.clone()),
        )
        // Automation rules (tag triggers, webhooks, etc)
        .nest("/api/automation", automation::router(state.clone()))
        // Tracked links
        .nest("/api/tracked-links", tracked_links::router(state.clone()))
        // Industry dashboards — the workspace's industry tabs. The module was declared (`pub mod
        // industries;`) and never nested, so all five routes 404'd and the `industries` number
        // `GET /api/auth/me/usage` reports could never leave 0 (kanban t_0986ba98). Its ceiling comes
        // from the admin-assignable `limit_max_industries` module feature, not `plans.max_industries`.
        .nest("/api/industries", industries::router(state.clone()))
        // Email Templates CRUD (admin only)
        .nest(
            "/api/email-templates",
            email_templates::router(state.clone()),
        )
        // Private email boxes (Mailgun integration, plan-gated)
        .nest("/api", tickets::router(state.clone()))
        .nest("/api/widgets", support_widgets::router(state.clone()))
        .nest("/api/private-email", private_email::router(state.clone()))
        // Public redirect for tracked links (no auth)
        .nest("/track", tracked_links::public_router())
        // Mailgun inbound webhook (no auth — called by Mailgun)
        .route(
            "/api/v1/webhooks/mailgun/inbound",
            axum::routing::post(private_email::webhook_handler::inbound_webhook).layer(
                axum::middleware::from_fn_with_state(
                    body_read_deadline,
                    body_deadline::body_read_deadline_middleware,
                ),
            ),
        )
        // NOTE: POST /api/billing/webhooks/{stripe,paypal} were RETIRED 2026-09-25 (kanban
        // t_0fe500d4) together with POST /api/billing/checkout/create. They authenticated nothing
        // (no signature, no shared secret) and their only effect was to complete a
        // `checkout_sessions` row — a table that no tenant ever had a row in and that migration 097
        // drops. Leaving them registered would have restored the 42P01 that t_a8a3fa27 had just
        // closed. See src/billing/handlers.rs for the full retirement note.
        // Public booking endpoints (no auth)
        .nest(
            "/api/public/bookings",
            bookings::public_router().layer(axum::middleware::from_fn_with_state(
                body_read_deadline,
                body_deadline::body_read_deadline_middleware,
            )),
        )
        // Tickets public endpoints (root level)
        .route(
            "/s/:tenant_id/ticket",
            axum::routing::post(tickets::handlers::public_submit_ticket).layer(
                axum::middleware::from_fn_with_state(
                    body_read_deadline,
                    body_deadline::body_read_deadline_middleware,
                ),
            ),
        )
        .route(
            "/s/:tenant_id/widget.js",
            axum::routing::get(tickets::handlers::support_embed_script),
        )
        // CS-22 — the customer-facing "💬 My Support" portal (public, capability-scoped: the grant
        // travels in the X-Support-Token header, never in the URL). Reached through nginx's
        // existing `location /s/` proxy on app.coreswiftcrm.com.
        .route(
            "/s/:tenant_id/support",
            axum::routing::get(tickets::portal::page),
        )
        .route(
            "/s/:tenant_id/support/",
            axum::routing::get(tickets::portal::page),
        )
        .route(
            "/s/:tenant_id/support/login",
            axum::routing::post(tickets::portal::login).layer(
                axum::middleware::from_fn_with_state(
                    body_read_deadline,
                    body_deadline::body_read_deadline_middleware,
                ),
            ),
        )
        .route(
            "/s/:tenant_id/support/tickets",
            axum::routing::get(tickets::portal::list)
                .post(tickets::portal::create_ticket)
                .layer(axum::middleware::from_fn_with_state(
                    body_read_deadline,
                    body_deadline::body_read_deadline_middleware,
                )),
        )
        .route(
            "/s/:tenant_id/support/tickets/:id",
            axum::routing::get(tickets::portal::detail),
        )
        .route(
            "/s/:tenant_id/support/tickets/:id/messages",
            axum::routing::post(tickets::portal::reply).layer(
                axum::middleware::from_fn_with_state(
                    body_read_deadline,
                    body_deadline::body_read_deadline_middleware,
                ),
            ),
        )
        .route(
            "/api/public/contact",
            axum::routing::post(tickets::handlers::public_contact_form).layer(
                axum::middleware::from_fn_with_state(
                    body_read_deadline,
                    body_deadline::body_read_deadline_middleware,
                ),
            ),
        )
        // Layer stack (inner to outer = last to first in call order)
        .layer(CompressionLayer::new())
        .layer(axum::middleware::from_fn(security_headers_middleware))
        .layer(request_id_middleware)
        .layer(TraceLayer::new_for_http())
        // General API rate limit (kanban t_3130f105). INSIDE Cors so a 429 still carries the CORS
        // headers a browser needs in order to read it, and outside the routing/nest layers so it
        // covers every API route. The middleware itself skips static paths, the health probes and
        // the machine-to-machine receivers.
        .layer(axum::middleware::from_fn_with_state(
            state.rate_limiter.clone(),
            crate::rate_limiter::api_rate_limit_middleware,
        ))
        // Default-deny credential boundary (kanban t_d8d782f2). Mounted OUTSIDE the routing/nest
        // layers so it decides before any module's own `auth_middleware` runs, and inside CORS so a
        // 401 still carries the CORS headers a browser needs to read it. It refuses only a caller
        // that presents no credential at all (`src/auth/boundary.rs`); the allowlist it reads is the
        // committed, unit-tested `src/auth/route_policy.rs`.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::auth::boundary::require_credential,
        ))
        .layer(CorsLayer::permissive())
        .with_state(state.clone());

    // Start background worker for delayed actions, inactive trials, health recalculation
    let db_for_worker = state.db.clone();
    tokio::spawn(async move {
        if let Err(e) = worker::start_worker(db_for_worker).await {
            tracing::error!(error = %e, "Failed to start background worker");
        }
    });

    let addr = format!("{}:{}", config.host, config.port);
    tracing::info!("Starting CRM Swift server on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await?;

    // Graceful shutdown with CTRL+C
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

/// The host-side applier (`crm-swift apply-site-settings`, driven by /opt/swift/bin/cs-site-apply.sh
/// from cron */5). It is the ONLY writer of /opt/swift/nginx/www/coreswift/*.
///
/// It prints one machine-readable summary line — `site artifacts: written=N skipped=M` — and one
/// line per file it touched or deliberately left alone, so the cron log and the card's proof can
/// both read what happened without a second probe.
///
/// `--check` renders and reports `state=unchanged|would-write` with both sha256s but writes
/// NOTHING, which is the instrument a reconciliation uses BEFORE the first real apply: the appliers
/// are idempotent only once the DB and the served bytes agree, so a row holding code defaults where
/// the page holds authored copy would rewrite the live homepage on the first run (the near-miss
/// caught on ADASwift, card t_1f427190). `--emit DIR` drops the rendered bytes elsewhere for a
/// byte-for-byte diff.
async fn apply_site_settings_mode() {
    use sha2::{Digest, Sha256};

    let args: Vec<String> = std::env::args().collect();
    let check = args.iter().any(|a| a == "--check");
    let emit_dir = args
        .iter()
        .position(|a| a == "--emit")
        .and_then(|i| args.get(i + 1))
        .cloned();

    let url = match std::env::var("DATABASE_URL") {
        Ok(u) => u,
        Err(_) => {
            eprintln!("apply-site-settings: DATABASE_URL is not set");
            std::process::exit(2);
        }
    };

    let db = match db::connect(&url, 1, 4).await {
        Ok(d) => d,
        Err(e) => {
            eprintln!("apply-site-settings: cannot connect to the database: {}", e);
            std::process::exit(1);
        }
    };

    let settings = match crate::admin_actions::site_handler::load_settings(&db).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "apply-site-settings: cannot read the site settings row: {:?}",
                e
            );
            std::process::exit(1);
        }
    };

    let (targets, skipped) = crate::admin_actions::site_handler::plan(&settings);

    // --emit: drop the rendered bytes somewhere else so they can be compared byte-for-byte with the
    // served file WITHOUT this process writing anything under SITE_ROOT.
    if let Some(dir) = emit_dir {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!(
                "apply-site-settings: cannot create --emit dir {}: {}",
                dir, e
            );
            std::process::exit(1);
        }
        for (path, rendered) in &targets {
            let name = std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "rendered".to_string());
            let dest = std::path::Path::new(&dir).join(name);
            if let Err(e) = std::fs::write(&dest, rendered.as_bytes()) {
                eprintln!(
                    "apply-site-settings: cannot write {}: {}",
                    dest.display(),
                    e
                );
                std::process::exit(1);
            }
            println!("emit {} -> {}", path, dest.display());
        }
    }

    if check {
        for (path, reason) in &skipped {
            println!("skip {} {}", path, reason);
        }
        for (path, rendered) in &targets {
            let now = std::fs::read_to_string(path).unwrap_or_default();
            let state = if now == *rendered {
                "unchanged"
            } else {
                "would-write"
            };
            println!(
                "check {} state={} sha256={} served_sha256={}",
                path,
                state,
                hex::encode(Sha256::digest(rendered.as_bytes())),
                hex::encode(Sha256::digest(now.as_bytes())),
            );
        }
        println!(
            "site artifacts (check, nothing written): targets={} skipped={}",
            targets.len(),
            skipped.len()
        );
        return;
    }

    let (written, skipped) = crate::admin_actions::site_handler::apply_to_disk(&settings);
    for path in &written {
        println!("write {} (the rendered bytes differ from the file)", path);
    }
    for (path, reason) in &skipped {
        println!("skip {} {}", path, reason);
    }
    // Exactly one machine-readable line for the cron log.
    println!(
        "site artifacts: written={} skipped={}",
        written.len(),
        skipped.len()
    );
}

/// Set when the boot migration run failed and `MIGRATIONS_FATAL=0` let this process boot anyway.
/// Reported in the health payload so an "up, but the schema is behind the code" container is
/// visible over HTTP and not only in its boot log (card t_10dd6fc7).
static MIGRATION_FAILURE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Whether a failed boot migration run should refuse to serve. Default is FATAL (anything other
/// than `0`/`false`); `MIGRATIONS_FATAL=0` is the single documented escape hatch. Fleet convention:
/// docs/fleet-migration-guard-convention-2026-09-21.md.
fn migrations_fatal() -> bool {
    std::env::var("MIGRATIONS_FATAL")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(true)
}

/// The sqlx error VARIANT name only (`VersionMismatch`, `VersionMissing`, `Dirty`, …). `MigrateError`
/// is `#[non_exhaustive]` and its Debug payload can be an entire `PgDatabaseError`, so the variant is
/// taken from the derived Debug prefix instead of formatting the whole thing into the log line.
fn migration_error_kind(e: &sqlx::migrate::MigrateError) -> String {
    format!("{:?}", e)
        .split('(')
        .next()
        .unwrap_or("MigrateError")
        .trim()
        .to_string()
}

/// Health check endpoint — returns 200 when the service is running
async fn health_check() -> impl IntoResponse {
    let mut body = serde_json::json!({
        "status": "ok",
        "service": "crm-swift",
        "version": env!("CARGO_PKG_VERSION")
    });
    // Present ONLY in the `MIGRATIONS_FATAL=0` case: the process is serving with migrations
    // UNAPPLIED, and a bare `status: "ok"` would hide exactly the condition this card is about.
    if let Some(error) = MIGRATION_FAILURE.get() {
        body["migrations"] = serde_json::json!("failed");
        body["migration_error"] = serde_json::json!(error);
    }
    (StatusCode::OK, axum::Json(body))
}

/// Readiness check endpoint — verifies database connectivity
async fn ready_check(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> impl IntoResponse {
    // Quick DB ping to verify connectivity
    let db_ok = sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.db)
        .await
        .is_ok();

    use redis::AsyncCommands;
    let mut redis_conn = state.redis.clone();
    let redis_ok: bool = redis_conn
        .set::<&str, &str, String>("healthcheck", "ok")
        .await
        .is_ok();

    if db_ok && redis_ok {
        (
            StatusCode::OK,
            axum::Json(serde_json::json!({
                "status": "ready",
                "database": "connected",
                "redis": "connected",
            })),
        )
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({
                "status": "not_ready",
                "database": db_ok,
                "redis": redis_ok,
            })),
        )
    }
}

/// Request ID middleware — generates and attaches a UUID to each request
async fn request_id_middleware_fn(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    let request_id = uuid::Uuid::new_v4().to_string();
    req.extensions_mut().insert(RequestId(request_id.clone()));

    tracing::Span::current().record("request_id", request_id.as_str());

    let mut response = next.run(req).await;
    response.headers_mut().insert(
        "X-Request-Id",
        HeaderValue::from_str(&request_id).unwrap_or_else(|_| HeaderValue::from_static("unknown")),
    );
    response
}

/// Request ID wrapper for extension storage
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

/// Security headers middleware (Helmet-like)
async fn security_headers_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> impl IntoResponse {
    let mut response = next.run(req).await;

    // Security headers
    response.headers_mut().insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert("X-Frame-Options", HeaderValue::from_static("DENY"));
    response.headers_mut().insert(
        "X-XSS-Protection",
        HeaderValue::from_static("1; mode=block"),
    );
    response.headers_mut().insert(
        "Referrer-Policy",
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    response.headers_mut().insert(
        "Permissions-Policy",
        HeaderValue::from_static("geolocation=(), microphone=(), camera=()"),
    );

    response
}

/// Graceful shutdown handler — listens for SIGTERM/SIGINT
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("Ctrl+C received, starting graceful shutdown");
        }
        _ = terminate => {
            tracing::info!("SIGTERM received, starting graceful shutdown");
        }
    }

    // Give in-flight requests time to complete
    tokio::time::sleep(Duration::from_millis(500)).await;
    tracing::info!("Server shutdown complete");
}
