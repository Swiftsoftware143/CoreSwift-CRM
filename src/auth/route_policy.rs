//! Default-deny routing for the mounted surface (kanban t_d8d782f2), precedent
//! `FunnelSwift/src/auth/route_policy.rs` @c997a45 and
//! `IncentiveSwift/src/security/route_policy.rs` @47c909a4.
//!
//! # The rule
//!
//! **A mounted route is PRIVATE unless it appears in one of the three lists below.** CoreSwift-CRM
//! had no global gate: 40 module routers each mounted their own `auth::middleware::auth_middleware`
//! layer and the rest relied on the handler remembering to check something. That is a class of
//! default-allow — a module that forgets the layer, or a route added to a module that has none, is
//! anonymous until somebody notices. [`crate::auth::boundary::require_credential`] is now the single
//! boundary every guarded path passes through, and it reads only this module.
//!
//! # The census (measured 2026-10-06, from source, then verified live with an anonymous probe)
//!
//! ```text
//!   359 mounted `.route(..)` calls / 270 distinct paths  (the 52 `nest(..)` targets resolved
//!        recursively, plus the 14 mounts main.rs makes itself), and one `nest_service("/")`
//!        for the SPA
//!   347 mounts on /api/** (a path mounted once per method or per verb pair appears more than
//!        once here; the distinct path count is 270 for the whole tree)
//!    12 mounts at the root: 9 inbound receivers + /track/:slug + two /s/:tenant_id/** duplicates
//!
//!   live, with NO credential, against 127.0.0.1:8084:
//!   324 of the 347 /api mounts answered 401 (or 405) — the per-module gate was doing its job
//!    26 deliberate anonymous entries  -> PUBLIC_ROUTES  (23 /api + 3 /inbound)
//!    15 service-to-service entries    -> INTERNAL_ROUTES (all /api, reached with the shared key)
//!     2 issued-API-key entries        -> API_KEY_ROUTES  (/api/external/**)
//!     9 served surfaces outside the boundary: the SPA mount, /track/:slug and the eight
//!        /s/:tenant_id/** support-portal mounts
//! ```
//!
//! **No route was found answering an anonymous caller by accident.** Every anonymous path is a
//! deliberate surface (a signup entry point, a public catalogue, a path-keyed receiver, a support
//! portal page). This app's contribution is therefore structural, and it closes three default-allow
//! shapes rather than a live leak:
//!
//! 1. `auth::middleware::auth_middleware` carried a blanket bypass —
//!    `if path.ends_with("/internal") || path.contains("/internal/") { next.run(req) }` — so ANY
//!    route whose path merely contains `/internal/` skipped the JWT check and was anonymous until
//!    its author remembered an `x-internal-key` check of their own. All 14 current handlers do
//!    check it (measured live: 8 answer 401 with the handler's own body, 6 deserialize the body
//!    first and answer 422 — i.e. an anonymous caller provably reaches the handler code). The 14
//!    routes are now NAMED in [`INTERNAL_ROUTES`] and the shared key is demanded at the boundary,
//!    so the fifteenth route under that prefix is private by default.
//!    The census above was taken over the 14 routes that existed then; the account door
//!    `POST /api/v1/internal/provision-free-account` joined the list after it (kanban t_e968e9ad)
//!    and verifies the same shared key in its own handler, so the count below is 15 and a route
//!    added under this prefix without an entry here stays private by default.
//! 2. `/inbound/**` was outside any boundary at all (a root-level `nest`, no middleware, the key in
//!    the path). It is now inside [`is_guarded_path`] with its three mounted shapes named in
//!    [`PUBLIC_ROUTES`], so a future sibling route there is private by default instead of anonymous.
//! 3. Template matching is **segment-exact** ([`matches_template`]), never `starts_with`, so a new
//!    route that merely shares a prefix with an allowlisted one (`/api/widgets/...` vs
//!    `/api/widgets/widgets/...`) is private until it is written down here.
//!
//! One NAMED GAP was found and is recorded rather than hidden: `/api/telnyx/{webhook,sms-webhook}`
//! verify neither a signature nor a shared key (an anonymous POST is answered `{"status":"ack"}`).
//! They cannot present a JWT, so refusing them at the boundary would break inbound SMS/voice; the
//! missing verification is carded separately (see the completion handoff) and the comment on the
//! entries says so.
//!
//! # Credentials accepted
//!
//! * **App JWT** — `Authorization: Bearer <jwt>`, HS256 over `JWT_SECRET`, `iss=coreswift`,
//!   `aud=coreswift-api` (the same `auth::middleware::verify_token` every handler uses). This is
//!   the only credential the boundary *verifies*.
//! * **`X-Internal-Key`** — the app's own `INTERNAL_SYNC_KEY`, for [`INTERNAL_ROUTES`] only,
//!   compared constant-time, and never accepted when the app has no key configured.
//! * **Issued personal API key** — `Authorization: Bearer <key>`, for [`API_KEY_ROUTES`] only. The
//!   key is opaque and its validation is a database read the handler already performs
//!   (`external_api::resolve_key` against `personal_api_keys`), so the boundary requires only that
//!   a non-empty bearer be present and leaves the decision where the credential lives.
//!
//! The boundary never *widens* a caller's reach: it decides nothing about tenancy, roles, or which
//! tenant a request may touch. It can only refuse a caller that presents no credential at all.
//!
//! # Adding a route
//!
//! Leave it out of all three lists and it is private. Add an entry only when the route must answer
//! a caller that presents no credential — and add the shape to the test module below, so the
//! decision and its reason are recorded with the code.

/// Routes that may be reached with NO credential at all.
///
/// Templates use axum's `:param` spelling and match by segment (see [`matches_template`]), so
/// `/api/webhook/:token/:action` accepts `/api/webhook/abc/create` but never
/// `/api/webhook/abc/create/extra`.
pub const PUBLIC_ROUTES: &[&str] = &[
    // --- liveness ----------------------------------------------------------------------------
    // Read by the fleet uptime watchdog, by this app's deploy script (which fails the deploy on
    // anything but 200) and by nginx. `/api/admin/health` is the operator's own probe — the
    // admin router mounts it on an explicit `public` sub-router ("except health check") and it
    // returns service+database status only, never tenant data.
    "/api/health",
    "/api/ready",
    "/api/admin/health",
    // --- account entry points -----------------------------------------------------------------
    // Signup/login/refresh/recovery: this is where a credential is created, so they are anonymous
    // by definition. Each carries its own rate-limit bucket (auth / password) mounted inside
    // `auth::router`, which is where the abuse control lives.
    "/api/auth/register",
    "/api/auth/login",
    "/api/auth/refresh",
    "/api/auth/forgot-password",
    "/api/auth/reset-password",
    // --- public catalogues ---------------------------------------------------------------------
    // The signup wizard populates its provider and industry pickers from these before a session
    // exists. Both read platform-level rows with no tenant column (`provider_keys` mounts
    // `/available-providers` on an explicit `public` sub-router; `industries` mounts `/available`
    // the same way).
    "/api/available-providers",
    "/api/industries/available",
    // --- public booking surfaces (a visitor has no session) -------------------------------------
    // The public booking page resolves a tenant by id and lists that tenant's open slots, and a
    // visitor books without an account. All three are scoped to the tenant named in the path/body
    // by the handler.
    "/api/public/bookings/public/checkout",
    "/api/public/bookings/public/slots/available/:tenant_id",
    "/api/public/bookings/public/slots/questions",
    // Public contact form on the marketing site.
    "/api/public/contact",
    // --- receivers whose own credential travels in the path -------------------------------------
    // `/api/webhook/{token}/{action}`: the token IS the credential — `webhook::handlers` looks the
    // automation webhook up by `webhook_token` and answers 401 for an unknown one ("No auth header
    // needed — the token identifies the tenant"). n8n and Hermes post here.
    "/api/webhook/:token/:action",
    // Unified Inbox receiver for the MD/IS fleet apps: fire-and-forget, no credential by design
    // (the sender is a sibling app that has no CoreSwift session); the handler resolves the tenant
    // from the payload it receives.
    "/api/messages/webhook",
    // Mailgun's inbound-parse receiver. Mailgun cannot present a JWT; the handler resolves the
    // destination mailbox from the recipient it is given and answers `received: false` otherwise.
    "/api/v1/webhooks/mailgun/inbound",
    // --- provider push receivers (their own channel/signature is the credential) -----------------
    // Google Calendar push: the channel id and resource id are the credential, checked by the
    // handler (measured live: an anonymous post is answered 400 "Missing X-Goog-Channel-ID").
    "/api/google-calendar/webhook",
    // The OAuth redirect target Google sends the browser back to — there is no session yet.
    "/api/google-calendar/oauth-callback",
    // Telnyx call/SMS event receivers. NAMED GAP, not a passed check: these verify NEITHER a
    // signature NOR a shared key (measured live 2026-10-06 — an anonymous POST is answered
    // `{"status":"ack"}`). They cannot present a JWT, so refusing them here would break inbound
    // SMS/voice; the missing verification is carded separately rather than hidden behind the
    // allowlist. See the completion handoff.
    "/api/telnyx/webhook",
    "/api/telnyx/sms-webhook",
    // --- public widget surfaces ------------------------------------------------------------------
    // `embed.js` is the script a customer's own site loads (keyed by the public tenant + widget
    // slug) and `submit` is the anonymous visitor's form post that script makes. Both are scoped
    // to the slug pair in the path by the handler.
    "/api/widgets/widgets/:tenant_slug/:widget_slug/embed.js",
    "/api/widgets/widgets/:tenant_slug/:widget_slug/submit",
    // --- inbound machine receivers (credential = the key prefix in the path) ---------------------
    // `inbound::router` is mounted at the root and authenticates by the key prefix in the URL
    // ("No auth middleware — authentication is via key_prefix in URL"; v1 additionally checks the
    // `x-satellite-key` header). Named here so a NEW route under `/inbound/` is private by default.
    "/inbound/:key_prefix/:event_type",
    "/inbound/v2/:key_prefix/:event_type",
    "/inbound/v3/:key_prefix/:event_type",
];

/// Service-to-service routes: reachable with the app's own shared key and nothing else.
///
/// These are the mounts whose handlers each check `x-internal-key` against `INTERNAL_SYNC_KEY`,
/// previously reached through `auth_middleware`'s blanket `/internal/` bypass. The key is now
/// demanded at the boundary *in addition to* the handler's own check, so a route added under one of
/// these prefixes without an entry here is refused as an ordinary private route instead of being
/// anonymous until its author remembers.
pub const INTERNAL_ROUTES: &[&str] = &[
    // Cross-app sync from the satellite apps (ADASwift, FunnelSwift, WorkflowSwift, ...).
    "/api/internal/contacts",
    "/api/internal/lists/:list_id/members",
    "/api/internal/tenants/lookup",
    "/api/internal/tags",
    "/api/internal/tags/assign",
    "/api/internal/tags/delete",
    "/api/internal/tags/list",
    // The same internal calendar router is mounted twice — `/api/internal/bookings` and
    // `/api/bookings/internal` (main.rs: "alternative internal calendar creation path outside auth
    // middleware"). Both mounts are named; both demand the key.
    "/api/internal/bookings/calendars",
    "/api/internal/bookings/slots/default",
    "/api/bookings/internal/calendars",
    "/api/bookings/internal/slots/default",
    "/api/portfolio/internal",
    // FunnelSwift's tag-provisioning webhook and the cross-app tag-sync receiver.
    "/api/v1/internal/tag-provision",
    "/api/v1/webhooks/cross-app/tag-sync",
    // The ACCOUNT door beside them (kanban t_e968e9ad): FunnelSwift asks this app to mint the
    // free account for a tagged lead. Same shared key; the handler verifies it itself as well.
    "/api/v1/internal/provision-free-account",
];

/// Routes authenticated by an ISSUED PERSONAL API KEY (`Authorization: Bearer <key>`), not a JWT.
///
/// The key is opaque; `external_api::resolve_key` hashes it and looks it up in `personal_api_keys`.
/// The boundary therefore only requires that a non-empty bearer be presented — the decision stays
/// where the credential lives, which is exactly the "never widen a caller's reach" rule.
pub const API_KEY_ROUTES: &[&str] = &["/api/external/lists", "/api/external/contacts"];

/// Is `path` (a concrete request path) one of the committed public templates?
pub fn is_public_route(path: &str) -> bool {
    PUBLIC_ROUTES.iter().any(|t| matches_template(t, path))
}

/// Is `path` one of the committed internal templates (shared key required)?
pub fn is_internal_route(path: &str) -> bool {
    INTERNAL_ROUTES.iter().any(|t| matches_template(t, path))
}

/// Is `path` one of the issued-API-key routes?
pub fn is_api_key_route(path: &str) -> bool {
    API_KEY_ROUTES.iter().any(|t| matches_template(t, path))
}

/// Does this request path belong to the surface the credential boundary covers?
///
/// `/api/**` is the API surface; `/inbound/**` is the machine-receiver surface (previously ungated
/// at any level). Everything else the router mounts is a SERVED surface — the SPA (`/`), the public
/// tracked-link redirect (`/track/:slug`) and the customer-facing support portal plus its widget
/// script (`/s/:tenant_id/**`). Those render for an anonymous visitor, read no credential and return
/// no tenant data of their own (`/s/:tenant_id/support` renders an empty shell; the X-Support-Token
/// that authorises the portal's JSON lives in a header and is checked by `tickets::portal`), so they
/// stay outside the boundary. The census of them is in the module docs and pinned by a test.
pub fn is_guarded_path(path: &str) -> bool {
    path == "/api"
        || path.starts_with("/api/")
        || path == "/inbound"
        || path.starts_with("/inbound/")
}

/// Does one template match one concrete path?
///
/// Segment-wise: the split lengths must agree and every template segment is either a `:param`
/// (any one non-empty segment) or the identical literal. Deliberately stricter than a string
/// prefix — `/api/widgetsX` is a different route and must not be caught, and a template can never
/// accidentally swallow a longer path.
fn matches_template(template: &str, path: &str) -> bool {
    let t: Vec<&str> = template.split('/').collect();
    let p: Vec<&str> = path.split('/').collect();
    if t.len() != p.len() {
        return false;
    }
    t.iter().zip(p.iter()).all(|(tseg, pseg)| {
        if tseg.strip_prefix(':').is_some() {
            !pseg.is_empty()
        } else {
            tseg == pseg
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{
        is_api_key_route, is_guarded_path, is_internal_route, is_public_route, matches_template,
        API_KEY_ROUTES, INTERNAL_ROUTES, PUBLIC_ROUTES,
    };

    /// One `nest(prefix, module::func(..))` in main.rs, with that module's source embedded at
    /// compile time and the router function to scan. Every mount in this app happens either
    /// directly in `main.rs` or through exactly one of these 52 nests (measured: the whole tree
    /// contains 52 `.nest(` calls and main.rs holds all of them), so two levels is the whole shape.
    struct Nest {
        prefix: &'static str,
        src: &'static str,
        func: &'static str,
    }

    const NESTS: &[Nest] = &[
        Nest {
            prefix: "/api/auth",
            src: include_str!("../auth/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/account",
            src: include_str!("../account/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/profile",
            src: include_str!("../profile/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/contacts",
            src: include_str!("../contacts/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/csv",
            src: include_str!("../csv_handler.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/internal/contacts",
            src: include_str!("../contacts_internal.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/companies",
            src: include_str!("../companies/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/pipelines",
            src: include_str!("../pipelines/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/tags",
            src: include_str!("../tags/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/scoring",
            src: include_str!("../scoring/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/lists",
            src: include_str!("../lists/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/internal/lists",
            src: include_str!("../lists_internal.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/messages",
            src: include_str!("../messages/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/internal/tenants",
            src: include_str!("../tenants_internal.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/internal/tags",
            src: include_str!("../tags/internal_handler.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/integrations",
            src: include_str!("../integrations/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/integration-center",
            src: include_str!("../integration_center.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api",
            src: include_str!("../provider_keys/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/external",
            src: include_str!("../external_api.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/personal-api-keys",
            src: include_str!("../personal_api_keys.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/analytics",
            src: include_str!("../analytics/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/ai",
            src: include_str!("../ai/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/campaigns",
            src: include_str!("../campaigns/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/billing",
            src: include_str!("../billing/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/audit",
            src: include_str!("../audit/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/events",
            src: include_str!("../events/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/comms",
            src: include_str!("../communications/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/native",
            src: include_str!("../native_apps/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/webhook",
            src: include_str!("../webhook/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/dashboard",
            src: include_str!("../dashboard/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/portfolio",
            src: include_str!("../portfolio/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/bookings",
            src: include_str!("../bookings/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/bookings/internal",
            src: include_str!("../bookings/mod.rs"),
            func: "internal_router",
        },
        Nest {
            prefix: "/api/internal/bookings",
            src: include_str!("../bookings/mod.rs"),
            func: "internal_router",
        },
        Nest {
            prefix: "/api/round-robin",
            src: include_str!("../round_robin/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/inbound",
            src: include_str!("../inbound/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/admin",
            src: include_str!("../admin_actions/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/checklists",
            src: include_str!("../checklists/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/monitoring",
            src: include_str!("../monitoring/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/notifications",
            src: include_str!("../notifications/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/telnyx",
            src: include_str!("../telnyx/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/v1/webhooks",
            src: include_str!("../webhooks/cross_app_tag_sync.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/google-calendar",
            src: include_str!("../google_calendar/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/automation",
            src: include_str!("../automation/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/tracked-links",
            src: include_str!("../tracked_links/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/industries",
            src: include_str!("../industries/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/email-templates",
            src: include_str!("../email_templates.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api",
            src: include_str!("../tickets/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/widgets",
            src: include_str!("../support_widgets.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/private-email",
            src: include_str!("../private_email/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/track",
            src: include_str!("../tracked_links/mod.rs"),
            func: "public_router",
        },
        Nest {
            prefix: "/api/public/bookings",
            src: include_str!("../bookings/mod.rs"),
            func: "public_router",
        },
    ];

    /// The body of `fn <func>`, assuming rustfmt puts the closing brace at column 0.
    fn fn_body(src: &'static str, func: &str) -> &'static str {
        let needle_a = format!("\nfn {func}(");
        let needle_b = format!("\npub fn {func}(");
        let start = match src.find(&needle_a).or_else(|| src.find(&needle_b)) {
            Some(i) => i,
            None => return "",
        };
        let open = match src[start..].find('{') {
            Some(i) => start + i,
            None => return "",
        };
        match src[open..].find("\n}") {
            Some(i) => &src[open..open + i],
            None => &src[open..],
        }
    }

    /// Every `.route("path"` literal in `text`, in source order, with a nested router's own `/`
    /// route normalised to the nest prefix itself (verified live 2026-10-06: `/api/contacts`
    /// answers 401 and `/api/contacts/` answers 404).
    fn route_literals(text: &'static str) -> Vec<&'static str> {
        let bytes = text.as_bytes();
        let needle = b".route(";
        let mut out = Vec::new();
        let mut i = 0usize;
        while i + needle.len() <= bytes.len() {
            if &bytes[i..i + needle.len()] == needle {
                let mut j = i + needle.len();
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                let mut k = j;
                if j < bytes.len() && bytes[j] == b'"' {
                    k = j + 1;
                    while k < bytes.len() && bytes[k] != b'"' {
                        k += 1;
                    }
                    out.push(&text[j + 1..k]);
                }
                i = if k > i { k } else { i + 1 };
            } else {
                i += 1;
            }
        }
        out
    }

    /// The mounts `main.rs` makes itself, inside its own `Router::new()` build. They are read from
    /// the same block the nest map above was read from, so a route added there moves the census too.
    fn main_own_routes() -> Vec<String> {
        const MAIN: &str = include_str!("../main.rs");
        let start = MAIN.find("let app = Router::new()").unwrap_or(0);
        let end = MAIN
            .find(".with_state(state.clone())")
            .unwrap_or(MAIN.len());
        route_literals(&MAIN[start..end])
            .into_iter()
            .map(|p| {
                if p == "/" {
                    String::new()
                } else {
                    p.to_string()
                }
            })
            .collect()
    }

    /// The whole mounted route set: main.rs's own mounts plus every nest target's.
    fn mounted_routes() -> Vec<String> {
        let mut out = main_own_routes();
        for n in NESTS {
            let body = fn_body(n.src, n.func);
            for path in route_literals(body) {
                let leaf = if path == "/" { "" } else { path };
                out.push(format!("{}{}", n.prefix, leaf));
            }
        }
        out
    }

    #[test]
    fn every_allowlist_entry_names_a_mounted_route() {
        let mounted = mounted_routes();
        assert!(
            mounted.len() > 250,
            "route census found only {} routes — the extractor is broken, not the allowlist",
            mounted.len()
        );
        for entry in PUBLIC_ROUTES
            .iter()
            .chain(INTERNAL_ROUTES.iter())
            .chain(API_KEY_ROUTES.iter())
        {
            assert!(
                mounted.iter().any(|m| m == entry),
                "{entry:?} is not a mounted route (the router's own spelling must match exactly)"
            );
        }
    }

    #[test]
    fn neither_list_has_duplicates() {
        for (name, list) in [
            ("PUBLIC_ROUTES", PUBLIC_ROUTES),
            ("INTERNAL_ROUTES", INTERNAL_ROUTES),
            ("API_KEY_ROUTES", API_KEY_ROUTES),
        ] {
            let mut sorted: Vec<&str> = list.to_vec();
            sorted.sort_unstable();
            let before = sorted.len();
            sorted.dedup();
            assert_eq!(before, sorted.len(), "duplicate entry in {name}");
        }
    }

    #[test]
    fn the_three_lists_are_disjoint() {
        for p in PUBLIC_ROUTES {
            assert!(
                is_guarded_path(p),
                "{p} is on the public list but outside the boundary"
            );
            assert!(!is_internal_route(p), "{p} is public AND internal");
            assert!(!is_api_key_route(p), "{p} is public AND an API-key route");
        }
        for p in INTERNAL_ROUTES {
            assert!(
                is_guarded_path(p),
                "{p} is internal but outside the boundary"
            );
            assert!(
                !is_public_route(p),
                "{p} is internal but answers a public template"
            );
            assert!(!is_api_key_route(p), "{p} is internal AND an API-key route");
        }
        for p in API_KEY_ROUTES {
            assert!(is_guarded_path(p));
            assert!(!is_public_route(p));
            assert!(!is_internal_route(p));
        }
    }

    /// Tenant data surfaces and the operator prefix must stay private. These are the routes whose
    /// exposure to an anonymous caller is the whole reason a credential boundary exists, so they
    /// get an explicit negative control rather than relying on the absence of an allowlist entry.
    #[test]
    fn tenant_and_operator_surfaces_are_not_public() {
        for p in [
            "/api/contacts",
            "/api/contacts/0a1b2c3d",
            "/api/companies",
            "/api/pipelines",
            "/api/deals",
            "/api/tags",
            "/api/lists",
            "/api/campaigns",
            "/api/dashboard/stats",
            "/api/analytics/contacts",
            "/api/portfolio/companies",
            "/api/portfolio/internal",
            "/api/native/apps",
            "/api/private-email/boxes",
            "/api/provider-keys",
            "/api/personal-api-keys",
            "/api/account/settings",
            "/api/auth/me",
            "/api/auth/invites",
            "/api/auth/me/usage",
            "/api/widgets",
            "/api/widgets/inboxes",
            // the operator surface
            "/api/admin",
            "/api/admin/tenants",
            "/api/admin/impersonate",
            "/api/admin/site",
            "/api/admin/email-config/test",
            "/api/admin/plans/free/features",
            // internal, but reached with the shared key and never anonymously
            "/api/internal/contacts",
            "/api/internal/tags/list",
            "/api/v1/internal/tag-provision",
            "/api/portfolio/internal",
            // a NEW route under a previously-bypassed prefix must be private, not a bypass
            "/api/internal/something-new",
            "/api/internal/tags/new",
            "/api/v1/internal/whatever",
            // a NEW sibling of an allowlisted prefix must not be swept in by a string prefix
            "/api/widgets/widgets/tenant/widget/embed.js/extra",
            "/api/public/bookings/public/slots/available",
            "/api/auth/register/confirm",
            // the issued-API-key surface is not anonymous
            "/api/external/lists",
            "/api/external/contacts",
        ] {
            assert!(!is_public_route(p), "{p} must be private by default");
        }
    }

    /// The other side: the surfaces the app's own signup flow, an anonymous visitor and the
    /// webhook senders need must stay reachable with no credential. A blanket gate is the failure
    /// mode this list exists to stop — it would take down signup, the public booking page, the
    /// support portal, the widget and every inbound receiver at once.
    #[test]
    fn public_surfaces_stay_public() {
        for p in [
            "/api/health",
            "/api/ready",
            "/api/admin/health",
            "/api/auth/register",
            "/api/auth/login",
            "/api/auth/refresh",
            "/api/auth/forgot-password",
            "/api/auth/reset-password",
            "/api/available-providers",
            "/api/industries/available",
            "/api/public/bookings/public/checkout",
            "/api/public/bookings/public/slots/available/canary-tenant",
            "/api/public/bookings/public/slots/questions",
            "/api/public/contact",
            "/api/webhook/abc123/create",
            "/api/messages/webhook",
            "/api/v1/webhooks/mailgun/inbound",
            "/api/google-calendar/webhook",
            "/api/google-calendar/oauth-callback",
            "/api/telnyx/webhook",
            "/api/telnyx/sms-webhook",
            "/api/widgets/widgets/acme/support/embed.js",
            "/api/widgets/widgets/acme/support/submit",
            "/inbound/abc/contact-sync",
            "/inbound/v3/abc/contact-sync",
        ] {
            assert!(is_public_route(p), "{p} must stay reachable anonymously");
            assert!(
                is_guarded_path(p),
                "{p} must be inside the boundary so it is a named decision"
            );
        }
    }

    /// The internal surface is reachable ONLY with the shared key, so it must never be on the
    /// public list, and its templates must be matched exactly.
    #[test]
    fn internal_routes_are_named_and_not_public() {
        for p in [
            "/api/internal/contacts",
            "/api/internal/tags/list",
            "/api/internal/bookings/calendars",
            "/api/bookings/internal/calendars",
            "/api/portfolio/internal",
            "/api/v1/internal/tag-provision",
            "/api/v1/webhooks/cross-app/tag-sync",
        ] {
            assert!(is_internal_route(p), "{p} must be an internal route");
            assert!(!is_public_route(p), "{p} must not be anonymous");
        }
        // ...but a sibling under the prefix that nobody named is an ordinary private route.
        assert!(!is_internal_route("/api/internal/tags/nope"));
        assert!(!is_internal_route("/api/internal/contacts/extra"));
        assert!(!is_internal_route("/api/internal"));
        assert!(!is_public_route("/api/internal"));
        // and the old blanket bypass is provably gone
        assert!(!is_public_route("/api/internal/anything"));
        assert!(!is_public_route("/api/v1/internal/anything"));
    }

    /// Segment matching, not string prefixing — the defect class this module closes: a
    /// `starts_with` arm lets ANY sibling under an allowlisted prefix answer anonymously.
    #[test]
    fn matching_is_segment_exact() {
        assert!(matches_template(
            "/api/webhook/:token/:action",
            "/api/webhook/abc/create"
        ));
        assert!(!matches_template(
            "/api/webhook/:token/:action",
            "/api/webhook/abc/create/x"
        ));
        assert!(!matches_template(
            "/api/webhook/:token/:action",
            "/api/webhook/abc"
        ));
        assert!(matches_template(
            "/api/widgets/widgets/:tenant_slug/:widget_slug/embed.js",
            "/api/widgets/widgets/acme/support/embed.js"
        ));
        assert!(!matches_template(
            "/api/widgets/widgets/:tenant_slug/:widget_slug/embed.js",
            "/api/widgets/widgets/acme/support/embed.js/x"
        ));
        assert!(!matches_template("/api/health", "/api/healthz"));
        assert!(!matches_template("/api/health", "/api/health/extra"));
        assert!(matches_template("/api/health", "/api/health"));
        assert!(!matches_template(
            "/api/admin/health",
            "/api/adminstration/health"
        ));
        // a `:param` never matches an empty segment
        assert!(!matches_template(
            "/api/webhook/:token/:action",
            "/api/webhook//x"
        ));
        assert!(!matches_template(
            "/inbound/:key_prefix/:event_type",
            "/inbound//sync"
        ));
    }

    /// The served surfaces are outside the boundary by construction. This pins the boundary itself
    /// so a change to `is_guarded_path` cannot silently pull the SPA, the tracked-link redirect or
    /// the customer support portal into (or drop the API out of) the credential gate.
    #[test]
    fn served_surfaces_are_outside_the_boundary() {
        for p in [
            "/",
            "/index.html",
            "/track/abc123",
            "/s/canary-tenant/support",
            "/s/canary-tenant/support/",
            "/s/canary-tenant/support/login",
            "/s/canary-tenant/widget.js",
            "/s/canary-tenant/ticket",
        ] {
            assert!(
                !is_guarded_path(p),
                "{p} is a served surface, not a guarded path"
            );
            assert!(!is_public_route(p));
        }
        assert!(is_guarded_path("/api/health"));
        assert!(is_guarded_path("/api"));
        assert!(is_guarded_path("/inbound/x/y"));
        // a path that merely starts with the letters of a guarded prefix is NOT swept in
        assert!(!is_guarded_path("/apix/thing"));
        assert!(!is_guarded_path("/inboundx/y"));
    }

    /// The census shape the module docs quote. If a route is added or removed anywhere in the
    /// tree the documented numbers move, so the doc gets re-read — and the allowlist's coverage
    /// claim gets re-checked against the new mount.
    #[test]
    fn the_census_shape_is_what_the_docs_say() {
        let mounted = mounted_routes();
        let api = mounted.iter().filter(|p| p.starts_with("/api")).count();
        assert_eq!(
            mounted.len(),
            359,
            "mounted route count moved — update the census in the module docs \
             (regenerate: python3 scripts/route-census.py)"
        );
        assert_eq!(
            api, 347,
            "the /api mount count moved — re-read the census in the module docs"
        );
        // ...and the 12 root-level mounts are the served surfaces plus the inbound receivers.
        assert_eq!(mounted.len() - api, 12);
    }
}
