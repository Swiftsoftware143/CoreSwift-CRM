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
//!   361 mounted `.route(..)` calls / 272 distinct paths  (the 52 `nest(..)` targets resolved
//!        recursively, plus the 15 mounts main.rs makes itself), and one `nest_service("/")`
//!        for the SPA
//!   349 mounts on /api/** (a path mounted once per method or per verb pair appears more than
//!        once here; the distinct path count is 272 for the whole tree)
//!    12 mounts at the root: 9 inbound receivers + /track/:slug + two /s/:tenant_id/** duplicates
//!
//!   +2 routes / +2 paths since (2026-10-06, account matching — migration 113): 363 mounted
//!        `.route(..)` calls / 274 distinct paths over 53 nests, 351 mounts on /api/**. Both new
//!        mounts (`/api/account-match/resolve`, `/api/account-match/duplicates`) were measured live
//!        with NO credential and each answered 401, i.e. private by default like the rest — they are
//!        on no allowlist, which is the whole decision.
//!
//!   +2 mounts / +2 paths since (2026-10-08, kanban t_87b857f8), the profile surface: the
//!        anonymous profile-picture read `GET /api/avatars/:user_id` — an `<img src>` carries no
//!        token, so it joins the branding logo on PUBLIC_ROUTES and returns only the bytes that user
//!        uploaded, keyed by an unguessable uuid — plus its authenticated twin `POST
//!        /api/profile/avatar` inside the profile router, which stays private. Now 367 mounted
//!        `.route(..)` calls, 355 mounts on /api/** (the doc's older 363/351 numbers predate the
//!        routes added before this one).
//!
//!   +1 mount / +1 path since (2026-10-10, kanban t_ac2fe688), the panel mass-delete:
//!        `POST /api/admin/tenants/bulk-delete` on the platform-admin router, the console's
//!        "Delete selected" control. Private by default — it is on no allowlist, and every arm was
//!        measured live: anonymous 401, a signed-in tenant `owner` 403, and the caller's own
//!        workspace / a portfolio row each answered 200 carrying a per-id refusal reason. Now 368
//!        mounted `.route(..)` calls, 356 mounts on /api/**.
//!
//!   -2 mounts since (2026-10-14, kanban t_f6bb2958), the embeddable support widget: the two
//!        public widget arms were mounted INSIDE the router that main.rs nests at `/api/widgets`,
//!        so they were reachable only at the DOUBLED non-canonical `/api/widgets/widgets/...` while
//!        the `/s/...` URL `POST /api/widgets` advertises answered 404. Both arms now live in
//!        `support_widgets::public_router`, merged at the ROOT next to the ticket routes, and both
//!        allowlist entries went with the mounts. Now 367 mounted `.route(..)` calls, 355 mounts on
//!        /api/** — and the public widget surface is a SERVED surface (below), not an entry here.
//!
//!   +1 mount / +1 path since (2026-10-29, kanban t_d841df4f), the failed-mail signup door:
//!        `POST /api/auth/initial-password` on the auth router's GUARDED group — the customer whose
//!        credentials mail never left sets the first password they will ever know, using the token
//!        the signup response already carried. Private by default (on no allowlist), so the two
//!        lists are untouched; measured live with no credential: 401. Now 368 mounted
//!        `.route(..)` calls (367 before this mount), 356 mounts on /api/**.
//!
//!   +1 mount / +1 path since (2026-10-30, kanban t_c5bec49c), the email-branding preview:
//!        `GET /api/account/branding/preview-body` on the account router, next to the logo writers.
//!        It answers with the message this workspace's branding produces, RENDERED by the mail path
//!        and not sent, so the console's Preview card stops mirroring `Branding::header_html` in
//!        JavaScript. The recipient fields come from the caller's own `users` row (never the
//!        request), and it is on no allowlist — private by default, measured live with no
//!        credential: 401. Now 369 mounted `.route(..)` calls, 357 mounts on /api/**.
//!
//!   +2 mounts / +2 paths since (2026-11-06): (a) kanban t_90845bc1, the branding preview SEND —
//!        `POST /api/account/branding/preview` on the account router, beside the renderer above. It
//!        mails the SAME rendered message to the CALLER'S OWN address — read from their `users` row,
//!        with no body and no recipient field, so it cannot be pointed at a third party — through
//!        the one queueing funnel (`outbound_messages`), because the pane proves what the mailer
//!        BUILDS and only an inbox proves what a customer's client RENDERS. Private by default, on
//!        no allowlist, measured live with no credential: 401. (b) `POST /api/public/support-request`
//!        (commit b741a10, the locked-out-customer form, PUBLIC_ROUTES) landed without moving this
//!        census, so it is recorded here to keep the pinned numbers honest. Now 371 mounted
//!        `.route(..)` calls, 359 mounts on /api/**.
//!
//!   +1 mount / +1 path since (2026-11-07, kanban t_53de22f5), the contact-page ANSWER read:
//!        `POST /api/public/support-answer` (commit 396c0cd — the person who wrote from the contact
//!        page reads the team's reply, matched on the reference they were given plus the address
//!        they filed with). It is mounted in `main.rs` itself, beside the filing arm, behind the
//!        body-read deadline, and named in PUBLIC_ROUTES. It shipped WITHOUT moving this census,
//!        which left both the documented numbers and the test that pins them one behind the tree —
//!        the standard gate is fmt/check/clippy and never runs `cargo test`, so nothing caught it.
//!        Re-counted from source at HEAD 895e482 / eead91b: 373 mounted `.route(..)` calls, 361
//!        mounts on /api/** (373 = 361 /api + the 12 root-level mounts; the 373rd/361st is
//!        `GET /api/billing/readiness`, the payment-readiness read the operator dashboard asks,
//!        kanban t_f11f71fb — a PROTECTED mount on the billing router, platform-admin gated inside
//!        the handler and therefore deliberately absent from PUBLIC_ROUTES). Measured live on
//!        127.0.0.1:8084 with NO credential: an empty JSON body answers 422 (missing field
//!        `ticket_id`) and a body with no content type answers 415 — i.e. the global layer lets it
//!        through exactly as PUBLIC_ROUTES declares, and it is not a 401 by accident.
//!
//!   +15 routes / +15 mounts (2026-11-11, the inbox's own records — B204/B205/B211): the private
//!        email router gained /messages, /messages/counts, /messages/drafts,
//!        /messages/:id (get/patch/delete), /messages/:id/ticket, /messages/:id/reply,
//!        /messages/:id/summarize, /members, /addresses, /folders, /folders/action, /labels,
//!        /labels/:id, /labels/:id/apply and /domains/:id/records. Every one is a tenant data
//!        surface, so none is named in PUBLIC_ROUTES: measured anonymous they answer 401.
//!        Re-counted from source now: 389 mounted `.route(..)` calls, 377 mounts on /api/**
//!        (389 = 377 /api + the 12 root-level mounts). The count is pinned by
//!        `the_census_shape_is_what_the_docs_say`.
//!
//!   +2 mounts / +2 paths (2026-11-11, kanban t_bd31809e), the sign-in address change: the request
//!        arm `POST /api/auth/email-change` on the auth router's GUARDED group (private by default, on
//!        no allowlist, measured live with no credential: the boundary answers
//!        `{"error":"Authentication required"}`) and its confirm twin
//!        `POST /api/auth/email-change/confirm`, anonymous by necessity and named in
//!        [`PUBLIC_ROUTES`] — the token in the mailed link IS the credential.
//!        RE-MEASURED from source, not incremented: this tree actually carried 391 mounted
//!        `.route(..)` calls / 379 on /api** BEFORE this card, i.e. two mounts more than the 389/377
//!        the census had pinned (the drift the paragraph above describes, repeated — the gate never
//!        runs `cargo test`, so nothing caught it). With these two mounts the measured counts are
//!        393 mounted `.route(..)` calls and 381 mounts on /api** (393 = 381 /api + the 12
//!        root-level mounts), and those are what `the_census_shape_is_what_the_docs_say` pins.
//!
//!   +2 mounts (2026-11-11, kanban t_b671a0f7), the company signature: `GET|PUT
//!        /api/private-email/signature` (Compose's preview + one mailbox's own choice) and
//!        `GET|PUT /api/private-email/signature/workspace` (the ONE writer of the workspace
//!        default). Both sit on the private-email router, i.e. behind the same auth middleware as
//!        every sibling there, so neither joins an allowlist. The census pin was RE-MEASURED
//!        again rather than incremented: with a `+` on top of a pin that had already drifted, the
//!        source holds 402 mounts / 389 on /api** (the test's own extractor resolves 398 / 386 —
//!        it cannot see four mounts the tokenizer does; both numbers are stated so the next reader
//!        does not "fix" one into the other).
//!
//!   +1 mount (2026-11-24, kanban t_990c386e — re-pinned by t_e9ab0027 because the gate does not run
//!        `cargo test`, so the merge that added the automation rule-engine routes shipped without
//!        moving this pin): one more `/api` mount on the automation surface. The extractor now
//!        resolves 561 mounted `.route(..)` calls / 547 on /api** (strategy unchanged: re-measured
//!        from the tree, never incremented).
//!
//!   +3 mounts / +3 paths (2026-11-25, kanban t_856aaef9 — the pin above had drifted again): the
//!        booking page's own PAYMENT step. `GET /api/booking/calendars/:id/payment-wiring` (what the
//!        owner pastes into their provider), its rotate arm
//!        `POST /api/booking/calendars/:id/payment-wiring/rotate`, and the provider's reporting
//!        receiver `POST /api/public/booking/payments/:provider` (named in [`PUBLIC_ROUTES`] — the
//!        provider's own signature is the credential). All three sit on `/api**`, so the root-level
//!        count is unchanged. RE-MEASURED from the tree with this extractor, never incremented:
//!        564 mounted / 550 on /api** / 14 root-level.
//!
//!   +3 mounts / +3 paths (2026-11-25, kanban t_856aaef9): the ADDRESS REDIRECTS router, shipped in
//!        cb7e3ca ("Redirects for every address an owner has") and INVISIBLE to this census until it
//!        joined `NESTS` — `main.rs` nested it, but a mount the table cannot see is a surface nobody
//!        reviews (and the count stayed where it was while the product grew, which is how the drift
//!        above went unnoticed). The three arms on `/api/domain-redirects` are the owner's own
//!        address list (GET), the save (PUT) and the remove (DELETE by host). Session-only: the
//!        router attaches `auth_middleware` itself and joins no allowlist, so a stranger and an
//!        issued key are both refused. Measured 567 mounted / 553 on /api** / 14 root-level.
//!
//!   +1 mount (2026-11-11, kanban t_5d1d6d31, pinned here because the pin had gone stale): the
//!        billing screen's one-read bill, `GET /api/billing/summary` (the single amount due, the
//!        one open plan line, and the line that ended on the last change). It sits on the billing
//!        router behind the same auth middleware as every sibling, so it joins no allowlist. The
//!        extractor now resolves 399 mounted / 387 on /api**.
//!
//!   RE-MEASURED (2026-11-11, kanban t_f75da95f): no product change, the INSTRUMENTS were wrong.
//!        The SendGrid receiver (kanban t_04db9228) was mounted as a nest of an INLINE
//!        `axum::Router::new()`, which neither census can follow: its leaf `.route("/inbound/:token",
//!        ..)` was read as a ROOT-level mount and the real path,
//!        `POST /api/v1/webhooks/sendgrid/inbound/:token`, never appeared — so the allowlist check
//!        failed on a route that DOES mount, and the root count carried a mount that does not exist.
//!        Fixing that exposed the rest of the gap recorded two paragraphs above: `/api/workspaces`
//!        has been mounted since c130701 but was missing from this module's `NESTS` table, which is
//!        why the extractor resolved fewer routes than `scripts/route-census.py`. Both were fixed in
//!        the instruments — the receiver is now the named `webhook_handler::sendgrid_router`, and
//!        the workspaces nest is in `NESTS` — never by moving an assertion to fit. At the SendGrid
//!        commit the extractor resolved 400 mounts (387 on /api**, 13 at the root, one of them the
//!        phantom above); it now resolves 403 mounted `.route(..)` calls, 391 on /api**, 12 at the
//!        root, 314 distinct paths, and the script agrees path for path (it prints one extra row:
//!        the SPA `nest_service("/*")`). The three routes this measurement added to the count — and
//!        the one it relocated from the root to its real path — all mount: `GET|POST
//!        /api/workspaces`, `PATCH|DELETE /api/workspaces/:id` and
//!        `POST /api/workspaces/:id/default` (anonymous: 401 "Authentication required" — mounted
//!        and private by default, on no allowlist), and the SendGrid receiver (anonymous with a
//!        multipart body: 401 "inbound webhook refused", i.e. the allowlisted handler answered,
//!        not a 404). These numbers are what `the_census_shape_is_what_the_docs_say` pins.
//!
//!   +1 mount (2026-11-20, mail readiness gate): `GET /api/private-email/readiness`. A feature that
//!        needs mail asks it before doing anything, so the answer has to come from the server that will
//!        actually send: per lane (ordinary mail, automation, inbound) it names the identity, whether it
//!        is ready, the reason and the next action. It sits on the private-email router behind the same
//!        auth middleware as every sibling there, so it joins no allowlist. The extractor resolves
//!        **529 mounted `.route(..)` calls / 515 on /api** (529 = 515 /api + the 14 root-level mounts),
//!        which is what `the_census_shape_is_what_the_docs_say` pins.
//!
//!   +1 mount (2026-11-20, booking readiness): `GET /api/booking/calendars/:id/readiness`. A calendar with no
//!        bookable resource or no opening hours is a public link that offers nothing, and from the owner's side
//!        that is indistinguishable from a broken product, so the screen asks the server what a customer would
//!        actually find. Behind the booking router's usual auth. The extractor resolves **531 mounted
//!        `.route(..)` calls / 518 on /api** (532 = 518 /api + the 14 root-level mounts).
//!
//!   +1 mount (2026-11-12, kanban t_f919924f), THE FILE A MESSAGE CARRIED: `GET
//!        /api/private-email/messages/:id/attachments/:attachment_id`. It sits on the private-email
//!        router behind the same auth middleware as every sibling, so it joins no allowlist: it
//!        resolves the file through the CALLER'S account AND the message, and is measured anonymous
//!        answering 401. The two send routes that now accept base64 file parts keep their mounts and
//!        only gain a scoped `DefaultBodyLimit`. The extractor resolves 413 mounted
//!        `.route(..)` calls / 401 on /api** (413 = 401 /api + the 12 root-level mounts), which is
//!        what `the_census_shape_is_what_the_docs_say` pins.
//!
//!   +1 mount (2026-11-12, kanban t_0b19a3cc, workspace switcher): `POST
//!        /api/workspaces/:id/switch` — the session re-mint that moves the caller between the
//!        account's workspaces. It sits on the workspaces router behind the same auth middleware,
//!        joins no allowlist (a bearer token is required), and is measured anonymous answering 401.
//!        The extractor resolves 414 mounted `.route(..)` calls / 402 on /api** (414 = 402 /api +
//!        the 12 root-level mounts), which is what `the_census_shape_is_what_the_docs_say` pins.
//!
//!   +2 mounts (2026-11-13, kanban t_ae02611a), CLOSE MY ACCOUNT: `POST /api/auth/close-account` on
//!        the auth router's GUARDED group (the caller's own row, read from the SESSION, with the
//!        password re-verified in the handler) and its fleet-contract alias `POST
//!        /api/v1/auth/close-account`, mounted once in `main.rs` with the SAME handler and the same
//!        `auth_middleware` because this app's customer auth lived under `/api/auth/*` and no
//!        /api/v1 auth tree existed then. (2026-11-26, David: that single alias was superseded by
//!        mounting the WHOLE auth router at `/api/v1/auth`, so `close-account` now rides that
//!        tree's guarded group and `main.rs` carries no standalone route for it.) Neither prefix
//!        joins an allowlist: both are measured anonymous answering 401 "Authentication required",
//!        i.e. the boundary refused before the handler ran.
//!        The extractor now resolves 424 mounted `.route(..)` calls / 412 on /api** (424 = 412 /api
//!        + the 12 root-level mounts), and those are what the census test pins. The last +8 are the
//!        data-mapping surface (`/api/data-mapping/*`, migration 155).
//!
//!   +2 mounts (2026-11-16, kanban t_e2ec5452), THE CUSTOMER'S OWN DATA COPY: `GET
//!        /api/account/export` on the account router (the customer's own copy of the workspace, one
//!        JSON document, account read from the token) and its fleet-contract alias `GET
//!        /api/v1/account/export`, mounted once in `main.rs` with the SAME handler and the same
//!        `auth_middleware`, because the other five apps all answer the /api/v1 path. Neither joins
//!        an allowlist: measured anonymous, both answer 401 "Authentication required" — the
//!        boundary refused before the handler ran. The extractor now resolves 472 mounted
//!        `.route(..)` calls / 460 on /api** (472 = 460 /api + the 12 root-level mounts), and those
//!        are what `the_census_shape_is_what_the_docs_say` pins.
//!
//!   +3 mounts (2026-11-20, kanban t_44c1aaa4), MULTI-CURRENCY: the rate table's read and manual
//!        refresh on the deals router (`GET /api/deals/fx-rates`, `POST /api/deals/fx-rates/refresh`
//!        — session JWT, admin for the refresh) and the scheduler's arm
//!        `POST /api/internal/fx-rates/refresh`, which demands the shared `x-internal-key` at the
//!        boundary (named in [`INTERNAL_ROUTES`]) and again in its handler. The extractor now
//!        resolves 520 mounted `.route(..)` calls / 506 on /api** (520 = 506 /api + the 14
//!        root-level mounts), and those are what `the_census_shape_is_what_the_docs_say` pins.
//!
//!   +1 mount (2026-11-20, kanban t_e2017eb0), THE TELNYX INBOUND SIGNING KEY:
//!        `GET|PUT /api/admin/telnyx-config` — the operator's second way in for the ACCOUNT Ed25519
//!        public key that both public Telnyx receivers verify deliveries with (before it, that key
//!        existed only as `TELNYX_PUBLIC_KEY` in the deploy environment, so inbound calls and SMS
//!        could not be turned on without a root file edit and a restart). It sits on the
//!        admin_actions protected branch, i.e. behind `auth_middleware` AND the router-wide
//!        platform-admin gate, so it joins no allowlist; measured anonymous it answers 401
//!        "Authentication required". The extractor now resolves 528 mounted `.route(..)` calls /
//!        514 on /api** (528 = 514 /api + the 14 root-level mounts), and those are what
//!        `the_census_shape_is_what_the_docs_say` pins.
//!
//!   live, with NO credential, against 127.0.0.1:8084:
//!   324 of the 347 /api mounts THAT EXISTED WHEN THIS CENSUS WAS TAKEN answered 401 (or 405) —
//!        the per-module gate was doing its job. The two /api routes added since (kanban
//!        t_e968e9ad: `/api/admin/provisioning-config` and
//!        `/api/v1/internal/provision-free-account`) were measured the same way and each answered
//!        401 anonymous; both are named in the lists below
//!    50 deliberate anonymous entries  -> PUBLIC_ROUTES  (47 /api + 3 /inbound) — COUNTS ARRAY
//!        ENTRIES: measured 27 (24 /api + 3 /inbound) before the confirm arm above was added,
//!        i.e. one more than the 26 this line used to claim, and 30 after the hosted calendar
//!        address added its two public arms (kanban t_8457bfef), and 31 after the per-calendar
//!        path added `/api/public/calendar/site/:slug` (kanban t_d4ac5433), and 44 once the
//!        receivers, portals and opt-out arms below were counted in full — plus 6 on 2026-11-26
//!        for the fleet-standard `/api/v1/auth/*` prefix (the SAME six anonymous doors the
//!        `/api/auth/*` tree already had, mounted a second time) = 50
//!    17 service-to-service entries    -> INTERNAL_ROUTES (all /api, reached with the shared key)
//!    14 issued-API-key entries        -> API_KEY_ROUTES  (/api/external/**, /api/data-mapping/events,
//!        and the custom-object REST + MCP surface — the one class that also accepts a console JWT,
//!        decided in `custom_objects::authenticate`)
//!    11 served surfaces outside the boundary: the SPA mount, /track/:slug, the eight
//!        /s/:tenant_id/** support-portal mounts and the two /s/:tenant_slug/widgets/** embed-widget
//!        mounts
//! ```
//!
//! **Two routes are deliberately in NONE of those lists**: `POST /api/auth/close-account` and
//! `POST /api/auth/erase-data` — a customer's own exit doors. They are session-only by design
//! (fleet standard docs/account-closure-erasure-standard-2026-11-24.md §3): a password is
//! re-verified in the handler and the typed word `CLOSE` / `DELETE` is judged there, so no issued
//! API key and no MCP tool can end or wipe an account. Pinned by
//! `closure_and_erasure_are_session_only_and_never_reachable_by_an_agent` below.
//!
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
//!    and verifies the same shared key in its own handler, so the count below is 17 (the last is
//!    the currency-rate refresh `POST /api/internal/fx-rates/refresh`, kanban t_44c1aaa4) and a route
//!    added under this prefix without an entry here stays private by default.
//! 2. `/inbound/**` was outside any boundary at all (a root-level `nest`, no middleware, the key in
//!    the path). It is now inside [`is_guarded_path`] with its three mounted shapes named in
//!    [`PUBLIC_ROUTES`], so a future sibling route there is private by default instead of anonymous.
//! 3. Template matching is **segment-exact** ([`matches_template`]), never `starts_with`, so a new
//!    route that merely shares a prefix with an allowlisted one
//!    (`/api/public/bookings/public/slots/available/:tenant_id` does NOT cover its own
//!    `/api/public/bookings/public/slots/available`, which is private) is private until it is
//!    written down here. **One exception, written as a trailing `*name`**: `/api/agent/*rest`, the
//!    forwarding agent door, which is one route by design and needs one entry. Every other template
//!    keeps the segment-exact rule, including `/api/agentX/...` — a bare prefix is still not a match.
//!
//! The one NAMED GAP this census found — `/api/telnyx/{webhook,sms-webhook}` verified neither a
//! signature nor a shared key, so an anonymous POST was answered `{"status":"ack"}` — is now CLOSED
//! (kanban t_fd5000e1). Both receivers verify Telnyx's Ed25519 `telnyx-signature-ed25519` over
//! `<telnyx-timestamp>|<raw body>` against the deployment's `TELNYX_PUBLIC_KEY`, enforce a freshness
//! window, and refuse a replay: 401 for a delivery that fails the check, 503 when this deployment has
//! no key configured at all. They remain on [`PUBLIC_ROUTES`] because a webhook receiver cannot
//! present a JWT — the signature is the credential, checked in the handler before anything is read
//! or written, which is what closes the gap without breaking inbound SMS/voice.
//!
//! The SECOND anonymous receiver that census named — `POST /api/v1/webhooks/mailgun/inbound` — was
//! a method-blind [`PUBLIC_ROUTES`] entry whose only gate (a per-domain Mailgun signing key) was
//! OPTIONAL: a domain with no key configured fell through the handler's reconstruction branch and
//! the delivery was processed anyway, so an anonymous caller could name a mailbox on it and create
//! contacts/tickets/events in that tenant (reproduced live 2026-10-11: HTTP 200 with one row in
//! each of the three tables). It is CLOSED (kanban t_7f134e77) in the same shape as Telnyx above:
//! the entry stays — Mailgun presents no session and cannot carry the shared key — and the
//! per-domain signing key is now MANDATORY. A delivery that does not validate, including one for a
//! domain that has no signing key configured at all, is answered 401 before any read of the body is
//! turned into a write. The unguessable-address variant was rejected: Mailgun is handed one fixed
//! route URL for every tenant, so it would mean re-provisioning each domain's route, and the
//! signing key is the credential Mailgun can actually carry.
//!
//!   +2 mounts (2026-11-18, kanban t_3f4c16a7), HOW A CUSTOMER PAYS: `GET|PUT
//!        /api/admin/billing-settings` (the operator's own payment details, on the platform-admin
//!        router — private by default, measured anonymous 401 and refused for a tenant owner by the
//!        router's platform-admin gate) and the ANONYMOUS price list `GET /api/v1/public/plans`,
//!        which the marketing pricing page reads with no session and therefore joins
//!        [`PUBLIC_ROUTES`] — the one deliberate anonymous entry this card adds. Both were measured
//!        live after the deploy. The extractor now resolves 474 mounted `.route(..)` calls / 462 on
//!        /api** (474 = 462 /api + the 12 root-level mounts), and those are what
//!        `the_census_shape_is_what_the_docs_say` pins.
//!
//! # Credentials accepted
//!
//! * **App JWT** — `Authorization: Bearer <jwt>`, HS256 over `JWT_SECRET`, `iss=coreswift`,
//!   `aud=coreswift-api` (the same `auth::middleware::verify_token` every handler uses). This is
//!   the only credential the boundary *verifies*.
//! * **`X-Internal-Key`** — the app's own `INTERNAL_SYNC_KEY`, for [`INTERNAL_ROUTES`] only,
//!   compared constant-time, and never accepted when the app has no key configured.
//! * **Issued personal API key** — `Authorization: Bearer *** for [`API_KEY_ROUTES`] only. The
//!   key is opaque and its validation is a database read the handler already performs
//!   (`external_api::resolve_key` against `personal_api_keys`), so the boundary requires only that
//!   a non-empty bearer be present — plus ONE property of the key itself: a `read_only` key
//!   (migration 210) is refused with **403** on every method/path not in
//!   [`READ_ONLY_KEY_ROUTES`]. That is the REST half of a role the MCP door already enforced, and
//!   it is an explicit allowlist: a route added later is refused until it is listed (kanban
//!   t_ce51cf34).
//!
//! Otherwise the boundary never *widens* a caller's reach: it decides nothing about tenancy or
//! which tenant a request may touch. It can refuse a caller that presents no credential at all, and
//! it can refuse a credential whose own `api_role` says "read only" on a write.
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
    // The web server's own question, asked before it serves a static guide: "is this request carrying a
    // live session?" It must answer anonymously (that is the point - the answer itself is the check), and
    // it reveals nothing beyond valid-or-not.
    "/api/auth/session/check",
    "/api/auth/refresh",
    "/api/auth/forgot-password",
    "/api/auth/reset-password",
    // The SECOND arm of the sign-in address change (kanban t_bd31809e). The customer opens the mailed
    // link in a browser and has NO session — the token in the query string IS the credential the
    // message just delivered, exactly like the reset link above. It writes nothing but that token's
    // own row (`users.email` of the user the token names, once). Its authenticated twin
    // (`POST /api/auth/email-change`) is deliberately NOT here: asking to move an address is an act on
    // the caller's own account and stays behind the session.
    "/api/auth/email-change/confirm",
    // THE FLEET-STANDARD PREFIX — the same anonymous doors again (David, 2026-11-26). `main.rs`
    // mounts the ONE auth router a second time at `/api/v1/auth`, so this app answers the path
    // every sibling app serves. Only the arms that are anonymous on the `/api/auth` tree are
    // repeated: signup, the credential, the refresh, the two recovery mails and the mailed
    // address-change confirmation. `/api/auth/logout` is deliberately absent on BOTH prefixes even
    // though the handler sits on the router's public group — it needs the caller's own token to
    // blacklist it, and the boundary answers 401 to a stranger on either path. The GUARDED half
    // (`/me`, `/invites`, `/invite`, `/team/*`, `/initial-password`, `/email-change`,
    // `/close-account`, `/erase-data`) is private by default on both prefixes and joins no
    // allowlist at either.
    "/api/v1/auth/register",
    "/api/v1/auth/login",
    "/api/v1/auth/refresh",
    "/api/v1/auth/forgot-password",
    "/api/v1/auth/reset-password",
    "/api/v1/auth/email-change/confirm",
    // --- public catalogues ---------------------------------------------------------------------
    // The signup wizard populates its provider and industry pickers from these before a session
    // exists. Both read platform-level rows with no tenant column (`provider_keys` mounts
    // `/available-providers` on an explicit `public` sub-router; `industries` mounts `/available`
    // the same way).
    "/api/available-providers",
    "/api/industries/available",
    // The ANONYMOUS PRICE LIST (kanban t_3f4c16a7). The marketing pricing page at
    // `nginx/www/coreswift/pricing.html` is read by a visitor with no session, and it carries the
    // operator's own `payment_instructions` ("how do I pay you?") beside the tiers — this app has no
    // card till at all, so those words are the only way money reaches the business. Narrow by
    // construction: active plans' slug/name/amounts/allowances and the operator's words, no ids, no
    // `checkout_url`, no raw feature JSON. Measured anonymous: 200.
    "/api/v1/public/plans",
    // --- public booking surfaces (a visitor has no session) -------------------------------------
    // The public booking page resolves a tenant by id and lists that tenant's open slots, and a
    // visitor books without an account. All three are scoped to the tenant named in the path/body
    // by the handler.
    "/api/public/bookings/public/checkout",
    "/api/public/bookings/public/slots/available/:tenant_id",
    "/api/public/bookings/public/slots/questions",
    // The HOSTED CALENDAR ADDRESS (kanban t_8457bfef). A visitor reaches `<workspace>.coreswiftcrm.com`
    // (or the workspace's own domain) with no session at all — that address IS the capability for
    // "show me this workspace's public calendar". Neither arm accepts a workspace id from the caller:
    // both resolve it from the request's `Host` header, so `GET` reads only calendars that are active
    // and `POST` writes a booking into the workspace the Host named.
    "/api/public/calendar/site",
    "/api/public/calendar/site/:slug",
    "/api/public/calendar/book",
    // Public contact form on the marketing site.
    "/api/public/contact",
    // The served contact page's filing arm (2026-11-06). Every support address this app printed lived
    // on a domain publishing MX = 0, so a locked-out customer's message bounced and they stayed locked
    // out. No credential, no workspace: ONE ticket with a NULL tenant, the address the visitor typed,
    // at most three per address per hour. A platform admin's queue labels it "Website visitor".
    "/api/public/support-request",
    // The READ twin of the arm above (2026-11-07): the visitor who filed that message cannot sign
    // in — that is why they used the page — so the operator's answer, which lives in
    // `ticket_messages`, was unreachable to them. The reference (the ticket's own uuid) plus the
    // address they filed with is the capability; every mismatch answers one neutral sentence.
    "/api/public/support-answer",
    // The per-tenant email-branding logo (kanban t_feab8aff). A MAIL CLIENT fetches this <img src>
    // with NO credential at all — a token-gated logo would simply never render. It returns one
    // thing: the image that tenant uploaded, keyed by an unguessable uuid; 404 when there is none.
    // The authenticated twin (`POST|DELETE /api/account/branding/logo`) stays private.
    "/api/branding/logo/:tenant_id",
    // The signed-in user's own profile PICTURE (kanban t_87b857f8). Same shape and same reason as
    // the branding logo directly above: an `<img src>` fetches this with no credential, and it
    // returns one thing — the picture that user uploaded, keyed by the unguessable user uuid; 404
    // when there is none. The authenticated twin (`POST /api/profile/avatar`) stays private.
    "/api/avatars/:user_id",
    // --- receivers whose own credential travels in the path -------------------------------------
    // `/api/webhook/{token}/{action}`: the token IS the credential — `webhook::handlers` looks the
    // automation webhook up by `webhook_token` and answers 401 for an unknown one ("No auth header
    // needed — the token identifies the tenant"). n8n and Hermes post here.
    "/api/webhook/:token/:action",
    // (The Unified Inbox receiver that used to sit here is now in INTERNAL_ROUTES below — it WRITES
    // a `cs_messages` row, so it is not an anonymous surface; kanban t_36cf12d0.)
    // Mailgun's inbound-parse receiver. Mailgun cannot present a JWT or the shared key, so the
    // credential lives in the handler: the per-domain Mailgun signing key, HMAC-SHA256-verified
    // over (timestamp + token) by `providers::mailgun::accept_inbound`. A delivery that does not
    // validate is REFUSED — including one for a domain with no signing key configured at all, which
    // used to be processed anyway and made such a domain an anonymous writer of contacts/tickets/
    // events (kanban t_7f134e77, reproduced live 2026-10-11).
    "/api/v1/webhooks/mailgun/inbound",
    // The same receiver at the OTHER shape customers copy from the events webhook (measured 2026-11-20:
    // a customer posted here and got 401 because nothing was mounted). Same handler, same authentication.
    "/api/webhooks/mailgun/inbound",
    // SendGrid's Inbound Parse receiver (kanban t_04db9228). SendGrid signs nothing — its inbound
    // parse has no HMAC and no shared-secret header — so the credential is the per-domain token in
    // the path: the handler resolves the domain BY that token (unique index) and refuses an unknown
    // one before a field is read. Same shape as `/api/webhook/:token/:action` above, where the token
    // identifies the tenant. It must stay anonymous because SendGrid presents no session and cannot
    // carry our internal key.
    "/api/v1/webhooks/sendgrid/inbound/:token",
    // --- provider push receivers (their own channel/signature is the credential) -----------------
    // Google Calendar push: the channel id and resource id are the credential, checked by the
    // handler (measured live: an anonymous post is answered 400 "Missing X-Goog-Channel-ID").
    "/api/google-calendar/webhook",
    // The OAuth redirect target Google sends the browser back to — there is no session yet.
    "/api/google-calendar/oauth-callback",
    // Telnyx call/SMS event receivers. Their credential is the delivery's own Ed25519 signature,
    // verified in the handler (`telnyx::verify`) before the event is read: `telnyx-signature-ed25519`
    // over `<telnyx-timestamp>|<raw body>` against the deployment's `TELNYX_PUBLIC_KEY`, plus a
    // freshness window and a replay guard — a delivery that fails is answered 401 (503 when no key is
    // configured here), so nothing unsigned is applied (kanban t_fd5000e1). Measured live 2026-10-06:
    // before, an anonymous `POST {}` reached the handler and was answered `{"status":"ack"}`; after,
    // the same POST is refused before any read. They cannot present a JWT, which is exactly why the
    // check lives in the handler and the entries stay here.
    "/api/telnyx/webhook",
    "/api/telnyx/sms-webhook",
    // --- the public widget surface is deliberately NOT here ---------------------------------------
    // It is a SERVED surface: `GET /s/:tenant_slug/widgets/:widget_slug/embed.js` and
    // `POST /s/:tenant_slug/widgets/:widget_slug/submit`, merged at the ROOT of the router by
    // main.rs (`support_widgets::public_router`), because that is the URL `POST /api/widgets`
    // advertises to the operator and the one BOTH nginx vhosts already proxy (`location /s/` in
    // sites/coreswift.conf — the same family as `/s/:tenant_id/widget.js` and the 💬 My Support
    // portal). `is_guarded_path` covers `/api/**` and `/inbound/**` only, so it is outside this
    // boundary by construction and needs no entry, exactly like those.
    // The two entries that used to sit here were the DOUBLED `/api/widgets/widgets/...` spelling of
    // arms mounted inside the router main.rs nests at `/api/widgets`: the nest prefix was the bug
    // (kanban t_f6bb2958). The advertised `/s/...` URL 404'd while a non-canonical doubled path
    // answered 200. Both entries and both mounts are gone — do NOT "fix" that by allowlisting the
    // single-prefix `/api/widgets/<tenant>/<widget>/submit` instead: a caller that guesses it is
    // refused 401 here, like any other unnamed /api path, and that is the intended answer.
    // --- inbound machine receivers (credential = the key prefix in the path) ---------------------
    // `inbound::router` is mounted at the root and authenticates by the key prefix in the URL
    // ("No auth middleware — authentication is via key_prefix in URL"; v1 additionally checks the
    // `x-satellite-key` header). Named here so a NEW route under `/inbound/` is private by default.
    "/inbound/:key_prefix/:event_type",
    "/inbound/v2/:key_prefix/:event_type",
    // --- booking engine, buyer-facing ----------------------------------------------------------
    // A buyer has no account here: the public booking pages are how a stranger reserves a slot and
    // pays. Six paths, nothing else — every admin route of the engine stays private by default.
    // The engine's own hold/expiry rules are what stop two buyers taking the same slot, so this
    // being anonymous cannot oversell anything.
    // what a customer may read of the knowledge base, for the account their widget names
    "/api/public/helpdesk/kb",
    // a stranger browsing the help centre: its menu, one article, and one vote
    "/api/public/helpdesk/kb/index",
    "/api/public/helpdesk/kb/article",
    "/api/public/helpdesk/kb/vote",
    "/api/public/booking/c/:slug",
    "/api/public/booking/c/:slug/availability",
    // WHAT THE PAGE IS SHOWING AND WHY (2026-11-23): the line that tells a visitor more dates open as
    // the calendar advances. Public because the page it describes is public.
    "/api/public/booking/c/:slug/limits",
    "/api/public/booking/c/:slug/form",
    "/api/public/booking/c/:slug/quote",
    "/api/public/booking/c/:slug/orders",
    // A PAYMENT PROVIDER REPORTS A COMPLETED SALE (2026-11-25). Anonymous because a provider holds no
    // session; the lock is the calendar's own key in `x-booking-payment-key`, checked in
    // `booking::handlers::public_payment_notice`, which also refuses a wrong amount or an unknown
    // reference. Declared here so the door is measurable rather than an accident of the path.
    "/api/public/booking/payments/:provider",
    "/api/public/booking/orders/:id",
    "/api/public/booking/orders/:order_id/bookings/:booking_id/creative",
    // The manage link a confirmation carries. The 64-character token IS the credential, so these
    // are anonymous by design and scoped to one order - a caller cannot reach anyone else's.
    // the root of a booking address: where to send a visitor who opened it with no calendar
    "/api/public/booking/host",
    "/api/public/booking/manage/:token",
    "/api/public/booking/manage/:token/cancel",
    "/api/public/booking/manage/:token/reschedule",
    "/api/public/booking/manage/:token/rebook",
    "/inbound/v3/:key_prefix/:event_type",
    // The opt-out itself (2026-11-19). Anonymous BY NECESSITY and the token in the path is the whole
    // credential: the person clicking it has no session, and the mailbox provider's one-click arm
    // (RFC 8058 `List-Unsubscribe-Post`) posts here with no human at all. It reads one signed token
    // (account + address + mail class) and writes one suppression row for THAT account — it can
    // neither read nor name another tenant's data, and it cannot lift a suppression.
    "/api/public/unsubscribe/:token",
    // The provider's event stream (delivered / bounced / complained / dropped). Anonymous because a
    // provider has no session; the HMAC signature over timestamp+token is the credential, verified
    // before a row is written. It attributes an event only to the account this app itself logged as
    // the sender of the message the event names — a body cannot choose a tenant.
    //
    // BOTH of these were first appended to `API_KEY_ROUTES` by mistake, which is why they answered
    // 401 "Authentication required" live: a route on the wrong list is anonymous in the docs and
    // private in production, and no unit test can tell the two apart. `anonymous_arms_are_on_the
    // _public_list` below pins the CLASS, not these two paths.
    "/api/webhooks/mailgun/events",
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
    // The DEPROVISION door (kanban t_c88f907f): a sibling fleet app takes back the workspace it
    // was provisioned (`multi-directory`'s delete door). `DELETE`, same shared key, and the
    // handler additionally requires the workspace's own slug as confirmation and refuses any
    // workspace that is not a machine-provisioned mirror (`tenant_purge`).
    "/api/internal/tenants/:id",
    "/api/internal/tags",
    "/api/internal/tags/assign",
    "/api/internal/tags/delete",
    "/api/internal/tags/list",
    // The currency rate table's daily refresh (kanban t_44c1aaa4): the fleet's n8n cron posts
    // here, and the handler verifies the same shared key. Named so the boundary demands it too.
    "/api/internal/fx-rates/refresh",
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
    // The Unified Inbox receiver (kanban t_36cf12d0). It was anonymous in PUBLIC_ROUTES and WRITES a
    // `cs_messages` row for the tenant it resolves from the payload — an anonymous caller could
    // inject messages into any tenant's inbox. The sender is a sibling fleet app (multi-directory),
    // so the shared key is its credential; the handler verifies it too.
    "/api/messages/webhook",
];

/// Routes authenticated by an ISSUED PERSONAL API KEY (`Authorization: Bearer <key>`), not a JWT.
///
/// The key is opaque; `external_api::resolve_key` hashes it and looks it up in `personal_api_keys`.
/// The boundary therefore only requires that a non-empty bearer be presented — the decision stays
/// where the credential lives, which is exactly the "never widen a caller's reach" rule.
pub const API_KEY_ROUTES: &[&str] = &[
    "/api/external/lists",
    "/api/external/contacts",
    // The DEAL WRITER (kanban t_2df68850): an outside system creates/updates a deal by its
    // external reference, moves its stage, and sets won/lost. The key resolves the workspace and
    // `external_ref` is unique per workspace, so a re-send updates rather than duplicating.
    "/api/external/deals",
    "/api/external/deals/:external_ref/stage",
    "/api/external/deals/:external_ref/status",
    // The data-mapping intake: a sibling app reports one event with the account's issued key. The
    // tenant comes from the key, never from the payload, so a caller cannot name another tenant.
    "/api/data-mapping/events",
    // REGISTERING A MAIL DOMAIN WITH A KEY (Giraudy CEO, 2026-11-27). The whole point of an account key is
    // to let the owner's own tooling run the account, and setting up mail is exactly that - but every
    // `/api/private-email/**` route was session-only, so the CEO's admin MCP key and the population key both
    // got 403 "this route needs a console session" and the last step of the mail setup could not be done by
    // the credential the fleet is meant to use. A key that may WRITE may register/verify a domain; a
    // `read_only` key is still refused by `refused_for_role`, which is the ONE place that decision is made.
    // The list/records arms are included because registering a domain is not one call: you add it, you read
    // the records to publish, then you ask for verification.
    "/api/private-email/domains",
    "/api/private-email/domains/:id",
    "/api/private-email/domains/:id/verify",
    "/api/private-email/domains/:id/records",
    // CUSTOM OBJECTS (migration 204) — the owned no-code backend. These are the ONE class of route
    // in this app that answers BOTH credential classes: `/api/external/**` is key-only, but an agent
    // (MCP) and an application (REST) must read the same rows, so the boundary here asks only for a
    // non-empty bearer and `custom_objects::authenticate` decides which credential it is — an issued
    // `csk_…` key, or a console session JWT. Whichever it is, it resolves to exactly one account and
    // the module reads the tenant from that resolution, never from the request.
    // THE AGENT DOOR (2026-11-25). An AI agent holds an issued key, and the console routes correctly
    // refuse one. Rather than widen a session door, these routes accept EITHER credential and resolve it
    // themselves (`agent_api::authenticate`), exactly as `/api/objects` does. A `read_only` key is still
    // The agent door is ONE forwarder: `/api/agent/<any console path>` authenticates the caller (key or
    // session) and re-issues the request against the console's own handler, so it is one route with a
    // wildcard tail — not thirteen named leaves. The tail is `*rest` in this app's axum 0.7 spelling, and
    // `matches_template` understands a trailing `*` as "one or more remaining segments"; the surfaces it
    // may not reach (`auth`, `personal-keys`, `api-keys`, `keys`, `mcp`, `internal`, and `agent` itself)
    // are refused inside `api_proxy::allowed`, and DELETE never passes the door at all.
    "/api/agent/*rest",
    "/api/mcp",
    "/api/objects",
    "/api/objects/:slug",
    "/api/objects/:slug/schema",
    "/api/objects/:slug/fields",
    "/api/objects/:slug/fields/:id",
    "/api/objects/:slug/records",
    "/api/objects/:slug/records/:id",
    "/api/objects/:slug/audit",
];

/// The ONLY (method, path) pairs a `read_only` issued key may reach (kanban t_ce51cf34).
///
/// A `read_only` key is a workspace key minus the pen: the role used to be enforced by the MCP door
/// alone, so the same key still created, updated and deleted through `/api/objects/**` and the write
/// arms of `/api/external/**`. The two doors must agree on what "read" means, so the REST half is
/// decided here — where the routes are already enumerated — rather than by a blanket
/// `method == GET`, because this app's issued-key surface also carries POSTs that ARE writes
/// (`/api/external/contacts`, `/api/data-mapping/events`). A route added later is refused by
/// DEFAULT (the safe direction) until somebody lists it here.
pub const READ_ONLY_KEY_ROUTES: &[(&str, &str)] = &[
    // The one read on the lead-capture surface an agent may make: what lists exist.
    ("GET", "/api/external/lists"),
    // Custom Objects — the four GET handlers on the REST door (`src/custom_objects/mod.rs`).
    ("GET", "/api/objects"),
    ("GET", "/api/objects/:slug/schema"),
    ("GET", "/api/objects/:slug/records"),
    ("GET", "/api/objects/:slug/audit"),
];

/// May a `read_only` issued key use this (method, path)?
///
/// Only the pairs in [`READ_ONLY_KEY_ROUTES`]; `HEAD` rides with `GET` (axum answers it from the
/// same handler). A trailing slash is normalised because `nest("/api/objects", …)` answers both
/// `/api/objects` and `/api/objects/`.
pub fn is_read_only_allowed(method: &str, path: &str) -> bool {
    let read = method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD");
    if !read {
        return false;
    }
    let trimmed = path.trim_end_matches('/');
    let path = if trimmed.is_empty() { "/" } else { trimmed };
    // A READ-ONLY KEY MAY READ ANYTHING THE KEY SURFACE SERVES (David, 2026-11-27). It used to be
    // limited to a narrow allowlist, and the result was incoherent: GET /api/external/contacts answered
    // 403 "this key is read-only — use a workspace key to write" — a credential named read-only refusing
    // to read. The allowlist is kept as the explicit arm, and any other path the key surface serves is
    // readable too; a WRITE is still refused, which is the only thing "read-only" should mean.
    READ_ONLY_KEY_ROUTES
        .iter()
        .any(|(m, t)| *m == "GET" && matches_template(t, path))
        || is_api_key_route(path)
}

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

/// Routes whose HANDLER is the credential boundary, so the global middleware must not answer first.
///
/// The MCP door (`POST /api/mcp`, `src/mcp_door`) resolves the credential itself on EVERY request —
/// hash + live `personal_api_keys` row + live workspace + the app's canonical session checks — and
/// answers a 401 as a JSON-RPC error (`-32001`), which is the only shape an MCP client can parse.
/// The global boundary runs BEFORE routing, so without this exemption its generic JSON body
/// (`{"error": "Authentication required"}`) replaces the protocol answer and the door can never pass
/// its own conformance probe. Nothing is widened: the door cannot serve a call without a resolved
/// caller, and it is still listed in [`API_KEY_ROUTES`], so the census keeps treating it as private.
pub const SELF_AUTHENTICATING_ROUTES: &[&str] = &["/api/mcp"];

/// Is this path served by a handler that resolves its own credential?
pub fn is_self_authenticating(path: &str) -> bool {
    SELF_AUTHENTICATING_ROUTES
        .iter()
        .any(|t| matches_template(t, path))
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
    // A trailing `*name` is the ONE prefix-shaped arm, and it exists for exactly one surface: the agent
    // door (`/api/agent/*rest`), which forwards every console path it is allowed to reach. It matches one
    // or more remaining segments, never zero (`/api/agent` alone is not a route) and never a bare
    // `starts_with` across a segment boundary (`/api/agentX/...` is a different path).
    if let Some(last) = t.last() {
        if last.starts_with('*') {
            let head = &t[..t.len() - 1];
            if p.len() <= head.len() {
                return false;
            }
            return head.iter().zip(p.iter()).all(|(tseg, pseg)| {
                if tseg.strip_prefix(':').is_some() {
                    !pseg.is_empty()
                } else {
                    tseg == pseg
                }
            });
        }
    }
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
        is_api_key_route, is_guarded_path, is_internal_route, is_public_route,
        is_read_only_allowed, is_self_authenticating, matches_template, API_KEY_ROUTES,
        INTERNAL_ROUTES, PUBLIC_ROUTES, READ_ONLY_KEY_ROUTES, SELF_AUTHENTICATING_ROUTES,
    };

    /// A `read_only` key is read-only on BOTH doors (kanban t_ce51cf34). Pin the REST half: the
    /// read arms answer, every write arm of `/api/objects/**` and `/api/external/**` refuses, and a
    /// GET on an unlisted path refuses too (the allowlist, not the method, is the authority).
    #[test]
    fn a_read_only_key_may_only_reach_the_read_arms() {
        // ── allowed ───────────────────────────────────────────────────────────────────────────
        assert!(is_read_only_allowed("GET", "/api/external/lists"));
        assert!(is_read_only_allowed("GET", "/api/objects"));
        assert!(is_read_only_allowed("GET", "/api/objects/"));
        assert!(is_read_only_allowed("HEAD", "/api/objects"));
        assert!(is_read_only_allowed("GET", "/api/objects/orders/schema"));
        assert!(is_read_only_allowed("GET", "/api/objects/orders/records"));
        assert!(is_read_only_allowed("GET", "/api/objects/orders/audit"));

        // ── Custom Objects, every write arm ───────────────────────────────────────────────────
        assert!(!is_read_only_allowed("POST", "/api/objects"));
        assert!(!is_read_only_allowed("PATCH", "/api/objects/orders"));
        assert!(!is_read_only_allowed("DELETE", "/api/objects/orders"));
        assert!(!is_read_only_allowed("POST", "/api/objects/orders/fields"));
        assert!(!is_read_only_allowed(
            "PATCH",
            "/api/objects/orders/fields/3f2a6c1e"
        ));
        assert!(!is_read_only_allowed(
            "DELETE",
            "/api/objects/orders/fields/3f2a6c1e"
        ));
        assert!(!is_read_only_allowed("POST", "/api/objects/orders/records"));
        assert!(!is_read_only_allowed(
            "PATCH",
            "/api/objects/orders/records/9b1c7d2e"
        ));
        assert!(!is_read_only_allowed(
            "DELETE",
            "/api/objects/orders/records/9b1c7d2e"
        ));

        // ── the /api/external write arms, plus the data-mapping intake ────────────────────────
        assert!(!is_read_only_allowed("POST", "/api/external/contacts"));
        assert!(!is_read_only_allowed("POST", "/api/external/deals"));
        assert!(!is_read_only_allowed(
            "PATCH",
            "/api/external/deals/quote-42/stage"
        ));
        assert!(!is_read_only_allowed(
            "POST",
            "/api/external/deals/quote-42/status"
        ));
        assert!(!is_read_only_allowed("POST", "/api/data-mapping/events"));

        // ── default deny: a path the issued-key surface does not serve is not a read ──────────
        assert!(!is_read_only_allowed("GET", "/api/external/anything"));
        // NOT default-denied any more: a GET to a path the key surface serves is a READ, whatever
        // method the surface actually carries. The router answers 405 for a method it does not serve,
        // so nothing is exposed - and refusing the read was the defect (measured: a read_only key on
        // GET /api/external/contacts answered 403 "this key is read-only - use a workspace key to
        // write"). "Read-only" may not mean "cannot read".
        assert!(is_read_only_allowed("GET", "/api/data-mapping/events"));
    }

    /// One `nest(prefix, module::func(..))` in main.rs, with that module's source embedded at
    /// compile time and the router function to scan. Every mount in this app happens either
    /// directly in `main.rs` or through exactly one of these nests (measured 2026-11-11: the whole
    /// tree contains 55 `.nest(` calls, main.rs holds all of them, and this table names all 55), so
    /// two levels is the whole shape. A nest whose target is not a named function — an inline
    /// `Router::new()` — cannot be resolved here and its leaf would be mis-read as a ROOT mount,
    /// which is why `private_email::webhook_handler::sendgrid_router` exists as a function.
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
        // The SAME auth router mounted a second time at the fleet-standard prefix (David,
        // 2026-11-26). Two rows, ONE router: the census must count both mount points, because the
        // credential boundary sees a request to either. A mount this table cannot see is a surface
        // nobody reviews.
        Nest {
            prefix: "/api/v1/auth",
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
            prefix: "/api/data-mapping",
            src: include_str!("../data_mapping/mod.rs"),
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
            prefix: "/api/account-match",
            src: include_str!("../account_match/mod.rs"),
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
            prefix: "/api/deals",
            src: include_str!("../deals/mod.rs"),
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
        // THE ACTION AGENT. It is censused like every other module, because an action that changes a
        // customer's record must not be able to arrive on a route the count does not know about.
        Nest {
            prefix: "/api/agent",
            src: include_str!("../agent/mod.rs"),
            func: "router",
        },
        // THE AGENT DOOR (kanban t_d618cd47). It shares the action agent's prefix — main.rs merges the
        // two routers into ONE nest — so it is listed as its own row: the census reads route literals
        // out of each module's own `router`, and a merged half the table cannot see is a keyed write
        // surface nobody counted. Every leaf here is on API_KEY_ROUTES.
        Nest {
            prefix: "/api/agent",
            src: include_str!("../agent_api/mod.rs"),
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
        // The booking engine mounts through `nest_service` (its routers carry their own middleware and
        // state), so its routes were invisible to this census — which is how "/api/public/booking/c/:slug"
        // came to sit on the public allowlist with nothing behind it as far as the extractor could see.
        // Listing them here keeps the allowlist checkable: the spelling in the list must match the
        // spelling in the router, and the customer-facing booking pages really are public.
        Nest {
            prefix: "/api/helpdesk",
            src: include_str!("../helpdesk/kb.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/helpdesk",
            src: include_str!("../helpdesk/canned.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/helpdesk",
            src: include_str!("../helpdesk/reports.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/helpdesk",
            src: include_str!("../helpdesk/attachments.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/helpdesk",
            src: include_str!("../helpdesk/generate.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/helpdesk",
            src: include_str!("../helpdesk/learn.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/booking",
            src: include_str!("../booking/mod.rs"),
            func: "admin_router",
        },
        Nest {
            prefix: "/api/public/booking",
            src: include_str!("../booking/mod.rs"),
            func: "public_router",
        },
        Nest {
            prefix: "/api",
            src: include_str!("../tickets/mod.rs"),
            func: "router",
        },
        // Plan / upgrade requests (kanban t_42872c18): the operator queue and the signed-in customer
        // door. TWO mounts, one module — the census must see both, because the credential boundary
        // sees a request to either. Both are private by default (on no allowlist).
        Nest {
            prefix: "/api/v1/plan-requests",
            src: include_str!("../plan_requests/mod.rs"),
            func: "customer_router",
        },
        Nest {
            prefix: "/api/v1/admin/plan-requests",
            src: include_str!("../plan_requests/mod.rs"),
            func: "admin_router",
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
        // The SendGrid inbound receiver. A nest of a NAMED router on purpose: while it was an
        // inline `Router::new()` at the mount this table could not see it, so the route count
        // drifted and the allowlist entry below named a path the extractor never produced
        // (kanban t_f75da95f).
        Nest {
            prefix: "/api/v1/webhooks/sendgrid",
            src: include_str!("../private_email/webhook_handler.rs"),
            func: "sendgrid_router",
        },
        // Workspaces. Mounted by main.rs since c130701 but absent from this table, which is why the
        // extractor resolved four mounts fewer than `scripts/route-census.py` did for months — the
        // gap the census paragraph above twice recorded as "the extractor cannot see four mounts".
        Nest {
            prefix: "/api/workspaces",
            src: include_str!("../workspaces/mod.rs"),
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
        // The hosted calendar address (kanban t_8457bfef). One module, two mounts: the console's own
        // address settings and the public page/book pair the hosted address serves.
        Nest {
            prefix: "/api/calendar-host",
            src: include_str!("../calendar_host/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/public/calendar",
            src: include_str!("../calendar_host/mod.rs"),
            func: "public_router",
        },
        // The email-compliance surface (2026-11-19). The opt-out arms are merged at the ROOT, so
        // their prefix is empty and the census reads their FULL paths — which is the point: an
        // anonymous route that the census cannot see is an anonymous route nobody reviews.
        Nest {
            prefix: "/api/mail-compliance",
            src: include_str!("../mail_compliance/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "",
            src: include_str!("../mail_compliance/mod.rs"),
            func: "public_router",
        },
        // Custom Objects (migration 204): the REST surface and, separately, the MCP door. Two rows
        // because they are two nests — `main.rs` mounts `/api/objects` and `/api/mcp`, and each
        // router's own literals are read from its own file.
        Nest {
            prefix: "/api/objects",
            src: include_str!("../custom_objects/mod.rs"),
            func: "router",
        },
        Nest {
            prefix: "/api/mcp",
            src: include_str!("../custom_objects/mcp.rs"),
            func: "router",
        },
        // Where an owner's own addresses send a visitor (kanban t_856aaef9, shipped in cb7e3ca). The
        // mount was invisible to this table, so the census could not see a surface that the customer
        // reaches: the three arms are the owner's own address list (GET), the save (PUT) and the remove
        // (DELETE by host). Session-only — the router attaches `auth_middleware` itself and is on no
        // allowlist, so a stranger and an issued key are both refused.
        Nest {
            prefix: "/api/domain-redirects",
            src: include_str!("../domain_redirects/mod.rs"),
            func: "router",
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

    /// MAINTENANCE TOOL: dumps the authoritative mounted route set to /tmp/mounted-routes.txt so the
    /// parity census (does every route have a screen and an MCP/API door?) can be measured against the
    /// SAME list the pinned count uses, instead of a regex over source that sees a fraction of it.
    #[test]
    fn dump_mounted_routes() {
        let mut out = mounted_routes();
        out.sort();
        out.dedup();
        std::fs::write("/tmp/mounted-routes.txt", out.join("\n")).ok();
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

    /// A route on the WRONG list is anonymous in the docs and private in production, and nothing
    /// else catches it: the mounted-route census passes either way and `cargo check` never looks.
    /// Measured 2026-11-19: the opt-out and the provider's event receiver were appended to
    /// `API_KEY_ROUTES` by mistake and answered 401 "Authentication required" on the live box while
    /// the code read as if they were open. This pins the CLASS.
    #[test]
    fn anonymous_surfaces_are_never_filed_as_key_or_internal_routes() {
        for entry in API_KEY_ROUTES.iter().chain(INTERNAL_ROUTES.iter()) {
            // `/public/` and `/unsubscribe` are customer-facing by definition: the person has no
            // session. (`/webhooks/` alone is NOT the signal — `/api/v1/webhooks/cross-app/tag-sync`
            // is a sibling-app push that legitimately carries an issued API key, so the provider
            // receiver is pinned by name in the test below instead.)
            assert!(
                !entry.contains("/public/") && !entry.contains("/unsubscribe"),
                "{entry:?} is an anonymous surface filed on a credential list — it will answer 401 \
                 while the docs say open"
            );
        }
    }

    /// The self-authenticating exemption must stay ONE path, must never be listed as anonymous, and
    /// must stay on the key list — otherwise a door that resolves its own credential would either be
    /// skipped by the boundary while claiming to be public (`/api/mcp` would then read as "open" in
    /// the census), or lose its own 401 to the middleware's generic body.
    #[test]
    fn the_self_authenticating_set_is_one_door_and_is_not_public() {
        assert_eq!(SELF_AUTHENTICATING_ROUTES, &["/api/mcp"]);
        assert!(is_self_authenticating("/api/mcp"));
        assert!(!is_self_authenticating("/api/mcp/x"));
        assert!(!is_self_authenticating("/api/objects"));
        assert!(
            !PUBLIC_ROUTES.contains(&"/api/mcp"),
            "/api/mcp authenticates every call; it must never be filed as an anonymous surface"
        );
        assert!(
            API_KEY_ROUTES.contains(&"/api/mcp"),
            "/api/mcp is a keyed surface and the census must keep saying so"
        );
    }

    /// The two arms a stranger or a provider must reach with NO credential: the one-click opt-out
    /// (RFC 8058) and the provider's event receiver. Their credential is the signed token / the HMAC
    /// signature, never a bearer.
    #[test]
    fn the_optout_and_the_provider_receiver_are_public() {
        assert!(super::is_public_route("/api/public/unsubscribe/abc.def"));
        assert!(super::is_public_route("/api/webhooks/mailgun/events"));
        assert!(!super::is_api_key_route("/api/public/unsubscribe/abc.def"));
        assert!(!super::is_internal_route("/api/webhooks/mailgun/events"));
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
        // The read-only allowlist is a SUBSET of the issued-key surface: an entry that is not on
        // API_KEY_ROUTES would be a rule nothing can ever reach (the boundary only applies it
        // there), and an entry that is not mounted would never match a real request.
        for (method, entry) in READ_ONLY_KEY_ROUTES {
            assert_eq!(*method, "GET", "{entry} rides a non-GET method");
            assert!(
                API_KEY_ROUTES.iter().any(|r| r == entry),
                "{entry:?} is read-only-allowed but is not on the issued-key surface"
            );
            assert!(
                mounted.iter().any(|m| m == entry),
                "{entry:?} is read-only-allowed but is not a mounted route"
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
            // Account matching reads and writes nothing but the caller's OWN tenant, but it is a
            // tenant data surface all the same: an anonymous caller must be refused at the boundary
            // (it is on no allowlist) and the module's own plan gate must answer, not this list.
            "/api/account-match/resolve",
            "/api/account-match/duplicates",
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
            // The profile surface is tenant data: the READ (GET /api/profile) and the writes must
            // stay private. The picture's anonymous READ is the deliberate exception above, and a
            // bodiless sibling of it (`/api/avatars`) or a deeper path is NOT swept in with it.
            "/api/profile",
            "/api/profile/password",
            "/api/profile/avatar",
            "/api/avatars",
            "/api/avatars/canary-user/extra",
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
            // The widget surface is public on `/s/...` ONLY — and that is a SERVED surface, outside
            // this boundary (see the test below). Its `/api/widgets/**` spelling, single or doubled
            // prefix, must stay refused here (kanban t_f6bb2958).
            "/api/widgets/acme/support/embed.js",
            "/api/widgets/acme/support/submit",
            "/api/widgets/widgets/acme/support/embed.js",
            "/api/widgets/widgets/acme/support/submit",
            // a NEW sibling of an allowlisted prefix must not be swept in by a string prefix
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
            "/api/v1/auth/register",
            "/api/v1/auth/login",
            "/api/v1/auth/refresh",
            "/api/v1/auth/forgot-password",
            "/api/v1/auth/reset-password",
            "/api/v1/auth/email-change/confirm",
            "/api/available-providers",
            "/api/industries/available",
            "/api/public/bookings/public/checkout",
            "/api/public/bookings/public/slots/available/canary-tenant",
            "/api/public/bookings/public/slots/questions",
            "/api/public/contact",
            "/api/public/support-request",
            "/api/public/support-answer",
            "/api/branding/logo/canary-tenant",
            "/api/avatars/canary-user",
            "/api/webhook/abc123/create",
            "/api/v1/webhooks/mailgun/inbound",
            "/api/v1/webhooks/sendgrid/inbound/6f1c2d3e4b5a69788796a5b4c3d2e1f0",
            "/api/google-calendar/webhook",
            "/api/google-calendar/oauth-callback",
            "/api/telnyx/webhook",
            "/api/telnyx/sms-webhook",
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
            "/api/messages/webhook",
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
            "/api/public/bookings/public/slots/available/:tenant_id",
            "/api/public/bookings/public/slots/available/canary-tenant"
        ));
        assert!(!matches_template(
            "/api/public/bookings/public/slots/available/:tenant_id",
            "/api/public/bookings/public/slots/available/canary-tenant/x"
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
        // The agent door is the ONE trailing-wildcard template: it covers the console paths it forwards,
        // never the nest itself, never zero segments, and never a prefix that is not a segment boundary.
        assert!(matches_template("/api/agent/*rest", "/api/agent/contacts"));
        assert!(matches_template(
            "/api/agent/*rest",
            "/api/agent/booking/calendars/abc/slots/generate"
        ));
        assert!(!matches_template("/api/agent/*rest", "/api/agent"));
        assert!(!matches_template(
            "/api/agent/*rest",
            "/api/agentX/contacts"
        ));
    }

    /// The served surfaces are outside the boundary by construction. This pins the boundary itself
    /// so a change to `is_guarded_path` cannot silently pull the SPA, the tracked-link redirect, the
    /// customer support portal or the public embed widget into (or drop the API out of) the
    /// credential gate.
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
            // The public embed widget (kanban t_f6bb2958). It is the URL `POST /api/widgets`
            // advertises and the one both nginx vhosts proxy, so it MUST be reachable with no
            // credential — and being a `/s/...` served surface is how that happens. No allowlist
            // entry exists for it on purpose: an entry would have to name a `/api/**` path, and the
            // only `/api/**` spelling of this surface is the non-canonical doubled one that was
            // retired.
            "/s/canary-tenant/widgets/support/embed.js",
            "/s/canary-tenant/widgets/support/submit",
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
    ///
    /// 2026-11-14: 424 → 451 → 454 mounted (412 → 439 → 442 on /api**). The first move made the
    /// booking routers visible; the second added the knowledge base (3 routes: list/create, get/update/
    /// delete, and the customer-facing search); the third added merge and split (+2), which live on the
    /// tickets router so they inherit its account scoping and its auth; the fourth added saved answers
    /// (+3: list/create, update/delete, and the render that fills an answer's placeholders); the fifth added
    /// the desk report (+1), which is a read that asks four questions of the ticket rows; the sixth added the
    /// attachments list (+1), which answers "which files came with this ticket?"; the seventh turned the
    /// knowledge base into a help centre (+5: two section routes, and the menu, one article and one vote a
    /// stranger can reach); the eighth added the draft generator (+1), which reads the account's own model key. Nothing was added to the tree: the
    /// booking engine's two routers (`.nest_service("/api/booking")` and `/api/public/booking`) joined
    /// `NESTS`, which is what made `"/api/public/booking/c/:slug"` checkable on the public allowlist at
    /// all. The instrument saw more routes because the product has more routes; the root-level count is
    /// unchanged at 12, which is the tell that no new surface was mounted outside /api**.
    ///
    /// +2 mounts / +2 paths (2026-11-20): the customer portal's emailed sign-in link — `/link` (mail one)
    /// and `/link/consume` (exchange it). Both are ROOT-level `/s/...` routes: that surface is OUTSIDE the
    /// credential boundary (like every other portal arm), which is why they are in no allowlist. 485
    /// mounted / 471 on /api** / 14 root-level.
    ///
    /// +6 mounts / +6 paths (2026-11-19): the email-compliance surface — 5 on
    /// `/api/mail-compliance` (the delivery evidence, the suppression list, its remove arm, and the
    /// two downloads) and 1 anonymous opt-out (`/api/public/unsubscribe/:token`, merged at the root
    /// and named in [`PUBLIC_ROUTES`] because the token in the path IS the credential). Every new
    /// path is on `/api**`, so the root-level count is unchanged at 12. +1 more for the provider's
    /// event stream (`/api/webhooks/mailgun/events`, signature-verified, anonymous).
    ///
    /// +32 mounts / +32 paths (2026-11-20): the DEAL WRITER (kanban t_2df68850) — 29 on
    /// `/api/deals` (the deal list/create, report, team, workspace currency, per-workspace custom
    /// field definitions, the configurable lost-reason list, stage automation hooks, and the
    /// per-deal detail/stage/status/notes/follow-ups/files arms) and 3 on `/api/external`
    /// (`POST /deals`, `PATCH /deals/:external_ref/stage`, `POST /deals/:external_ref/status` —
    /// the issued-key writer an outside system calls). Every new path is on `/api**`, so the
    /// root-level count is unchanged at 14. 517 mounted / 503 on /api**.
    /// +1 mount / +1 path (2026-11-24): the customer's own DATA ERASURE door
    /// (`POST /api/auth/erase-data`, kanban t_48b65042) — on `/api**`, so the root-level count is
    /// unchanged. 559 mounted / 545 on /api**.
    /// +1 mount / +1 path (2026-11-24): `DELETE /api/objects/:slug` — archiving a Custom Object
    /// (kanban t_933dd588 §6a) — on `/api**`, so the root-level count is unchanged.
    /// 560 mounted / 546 on /api**.
    ///
    /// +13 mounts / +13 paths (2026-11-25, kanban t_d618cd47): THE AGENT DOOR — the write surface an
    /// AI agent holding an issued `csk_…` key calls, each leaf wrapping the console handler it names:
    /// contacts (create, update), deals (create, update, move stage, add task), list members, tag
    /// assignment, the two calendar writers, the calendar's payment-key rotation, the resource's
    /// payment-checkout arm, and the address redirect — 13 leaves in all, measured from
    /// `agent_api::router`. It shares the action agent's `/api/agent` prefix: main.rs MERGES the two
    /// routers into one nest, and `NESTS` carries a row for each half, so the merged half is counted
    /// rather than invisible. Every leaf is on `/api**`, so the 14 root-level mounts are unchanged.
    /// 576 mounted / 562 on /api** — eight leaves moved this pin. Four of them are this lane's and are
    /// named here so the next reader can check them: `/api/contacts/custom-fields`,
    /// `/api/contacts/custom-fields/:id`, `/api/contacts/:id/fields` (the contact custom-field manager) and
    /// `/api/booking/bookings/:id/cancel` (the operator's cancel by booking id). The other four arrived from
    /// the booking sponsor work and other lanes before this census; the test's own walk is what counts them.
    /// 597 mounted / 583 on /api** (2026-11-26, kanban t_d7474aee): `DELETE /api/admin/users/:id` — the
    /// admin delete-user door. One leaf, on `/api**`, so the 14 root-level mounts are unchanged.
    /// 595 mounted / 581 on /api** (2026-11-26, kanban t_7d182634, David's fleet-standard auth
    /// path): the SAME `auth::router` mounted a second time at `/api/v1/auth` — its 18 leaves are
    /// now counted at both prefixes (+18) — and the standalone `POST
    /// /api/v1/auth/close-account` route retired from `main.rs`, because that router's guarded
    /// group carries the same handler behind the same `auth_middleware` (-1) = +17. Every new path
    /// is on `/api**`, so the 14 root-level mounts are unchanged. `/api/auth/*` was NOT removed:
    /// it is a permanent alias, and 22 files (the served login pages and the admin console among
    /// them) still call it.
    /// 603 mounted / 589 on /api** (2026-11-27, kanban t_42872c18, the plan-request queue):
    /// `GET`+`PATCH /api/v1/admin/plan-requests[/:id]` (the cross-tenant operator queue) and
    /// `POST /api/v1/plan-requests` (the signed-in customer door) — three leaves, all on `/api**`,
    /// so the 14 root-level mounts are unchanged.
    #[test]
    fn the_census_shape_is_what_the_docs_say() {
        let mounted = mounted_routes();
        let api = mounted.iter().filter(|p| p.starts_with("/api")).count();
        assert_eq!(
            mounted.len(),
            603,
            "mounted route count moved — update the census in the module docs.\
             The number to pin is the one THIS assertion reports as `left`: mounted_routes() walks the \
             router itself. scripts/route-census.py is a partial PARSER and a LOWER BOUND (it missed 23 \
             mounts on 2026-11-25); never pin its total."
        );
        assert_eq!(
            api, 589,
            "the /api mount count moved — re-read the census in the module docs"
        );
        // ...and the 14 root-level mounts are the served surfaces plus the inbound receivers.
        assert_eq!(mounted.len() - api, 14);
    }

    /// FLEET STANDARD §6 (docs/account-closure-erasure-standard-2026-11-24.md) — the exit doors are
    /// a signed-in HUMAN's alone, and that is structural, not promised.
    ///
    /// A machine credential must never be able to end or wipe a customer's account even when its
    /// agent is asked to: the two routes take a session only, and no allowlist entry may swallow
    /// them by template. The Custom Objects MCP surface (the fleet's one agent-facing tool list on
    /// this app) must expose no closure/erasure tool — its nine tools are objects, fields and
    /// records only. If anyone ever adds `erase_account` there, this test goes red before a
    /// customer's agent can call it.
    #[test]
    fn closure_and_erasure_are_session_only_and_never_reachable_by_an_agent() {
        const EXIT_DOORS: [&str; 2] = ["/api/auth/close-account", "/api/auth/erase-data"];
        for path in EXIT_DOORS {
            assert!(
                !is_api_key_route(path),
                "{path} must not be an issued-API-key route"
            );
            assert!(
                !is_internal_route(path),
                "{path} must not be a shared-key internal route"
            );
            assert!(!is_public_route(path), "{path} must not be anonymous");
            assert!(is_guarded_path(path), "{path} must sit inside the boundary");
            // No allowlist ENTRY (as a template) may match the door either: a `/api/auth/:x`
            // style entry would swallow it without the literal path ever appearing in the list.
            for entry in API_KEY_ROUTES.iter().chain(INTERNAL_ROUTES.iter()) {
                assert!(
                    !matches_template(entry, path),
                    "allowlist entry '{entry}' swallows the exit door {path}"
                );
            }
        }

        // The agent-facing tool list on this app names objects, fields and records — never the
        // account itself.
        const FORBIDDEN_FRAGMENTS: [&str; 6] = [
            "close",
            "erase",
            "purge",
            "delete_account",
            "close_account",
            "erase_data",
        ];
        const EXPECTED_TOOLS: [&str; 11] = [
            "list_objects",
            "create_object",
            "describe_object",
            "archive_object",
            "add_field",
            "archive_field",
            "insert_record",
            "set_unique_together",
            "query_records",
            "update_record",
            "delete_record",
        ];
        assert_eq!(
            crate::custom_objects::mcp::TOOL_NAMES,
            EXPECTED_TOOLS,
            "the Custom Objects tool list changed — it must stay objects/fields/records only"
        );
        for name in crate::custom_objects::mcp::TOOL_NAMES {
            let lower = name.to_ascii_lowercase();
            for bad in FORBIDDEN_FRAGMENTS {
                assert!(
                    !lower.contains(bad),
                    "Custom Objects exposes '{name}', which names a closure/erasure action ('{bad}')"
                );
            }
        }
    }
}
