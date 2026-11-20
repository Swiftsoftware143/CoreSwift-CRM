# Ticketing suite — audit, decisions, build order

Written 2026-11-18 so a NEW session can pick this up without the chat history. Read this first; then
`docs/automation-sending-lane.md` for the custom-domain lanes that sit beside it.

## Item 0 — the audit (done; do not redo)

| Required by the brief | State | Where |
|---|---|---|
| SLA countdown timers per status | **EXISTS** — policies per priority stamp `first_response_due` / `resolution_due` (migration 163), a minute worker warns an hour before and records each breach in a ledger (165, `helpdesk/sla.rs`), console shows the clocks and a "Due soon" view. **Gap:** clocks are keyed to response and resolution only — there is no time-in-status clock. | `src/helpdesk/sla.rs` |
| Internal notes hidden from the customer | **EXISTS** — `tickets.kind` (162); mail gated on kind not author; notes excluded from the portal thread, the portal count and the public lookup. Proved 12/12. | `src/tickets/handlers.rs`, `src/tickets/portal.rs` |
| Sentiment tags | **MISSING** — nothing in `src/` or `migrations/` mentions sentiment. | — |
| Customer status page | **EXISTS but not as specified** — `src/tickets/portal.rs` (388 lines): list, detail, reply, create; scoped to the customer's own records. **Login is an address-based capability proof, not an emailed one-time link** (the file says transactional mail was blocked on a missing credential when it was built). **No tenant branding.** | `src/tickets/portal.rs` |
| Per-tenant AI keys (item 5) | **MOSTLY EXISTS** — `provider_keys` stores per-tenant AI keys sealed at rest; `model_keys` reads them. **Missing:** platform-key fallback, and AI usage tracked per tenant. | `src/provider_keys/mod.rs` |
| Telnyx for escalation (item 2) | **EXISTS** — `src/telnyx/`. No new provider needed. | `src/telnyx/` |
| Action agent (item 1) | **MISSING** — nothing exists. | — |

So the plan adds: sentiment, at-risk escalation, the customer panel, the emailed-link status page, the action agent,
plus the two gaps above (time-in-status clock is optional; portal branding and login are required by item 4).

## Decisions already taken (do not re-litigate)

1. **Status page login**: build the emailed one-time link (single use, short expiry, never in the URL) AND keep the
   existing address-based capability proof underneath as the fallback for tenants with no mail configured. A login
   that depends on a credential the tenant may not have bought turns a support page into a support ticket.
2. **Money limits**: a limit **per action**, capped by a **tenant-wide ceiling**. An action's limit can never exceed
   the ceiling, and anything above either number goes to a person.
3. **Lifetime value is DERIVED, never stored** — CoreSwift has no LTV column, and a copy would drift. Compute it
   from won deals and settled invoices, and label it as at the moment it was read.
4. **Plan tier** is read from the same source billing uses (`tenant_plans → plans.slug`), so the panel and the bill
   cannot disagree.
5. **The customer joins by email** (`tickets.contact_email`), which is how every ticket already identifies a person.

## Build order

Custom-domain work first (it is the live gap and holds no migration): the Domains screen with three lanes, then the
automation send path — see `docs/automation-sending-lane.md`.

Then the ticketing suite in the order David set: **3 (customer panel) → 2 (at-risk escalation) → 5 (BYOK) →
4 (status page) → 1 (action agent, last because it is riskiest)**.

Working rules he set for this work: a new branch (never main), build and test after each step, **stop and show him
before any database migration**, no secrets in code/docs/commits, tenant isolation proved by a test, **no deploy**.

## Open item to settle BEFORE building on the branch — RESOLVED 2026-11-19

The drift was smaller than the counts suggested, and it is now reconciled on `feat/ticketing-suite`
(commit `0820a82`, gate green: fmt + check + clippy):

- `origin/main` is **fully contained** in the branch (0 commits on main missing from the branch). The
  only commit that was not, `e00c7b1` ("the help guide no longer stands in front of the login"), lived
  on the **stale local `main`** and was merged in. Local `main` has been pointed back at `origin/main`
  so the counts are honest again: branch is **8 ahead, 0 behind**.
- Real byte drift was found, and it was in the repo not the served root: `www-admin/inline-guide-widget.js`
  was 1,198 bytes behind the file nginx actually serves. Nothing served the stale copy, so no user saw
  it — but it was a dead twin waiting for the next session to edit "the widget". Both repo copies are now
  byte-identical to the served roots (`www-admin/`, `www-app/coreswift/`), which is the state the
  frontend-mirror gate expects.
- `main` was deliberately **not** moved to the branch tip: it is the deploy branch, and David's rule for
  this work is branch-only, no deploy. The suite therefore sits on a branch that contains everything
  main has, plus the shipped work main never received.

Re-check before building: `git rev-list --left-right --count main...HEAD` must read `0 <n>`.

## Where the compliance work stands (2026-11-20)

Email compliance is BUILT and LIVE on `app.coreswiftcrm.com`; every claim below was proved against the
running container and the live database, not read off the code.

| Piece | State | Proof |
|---|---|---|
| One gate in front of every send (opt-out, allowance, footer, one-click headers, send log) | live | `providers::deliver` is the single chokepoint; attempts logged in `email_message_log` |
| Provider reports (delivered / bounced / complained / failed) | live | real Mailgun reports recorded and attributed; a real permanent failure suppressed that address for that account only |
| Opt-out page (no login) | live | full document; invalid link answers 410 with guidance |
| Campaign checkboxes on that page | live | temporary campaign showed by name; one-campaign opt-out stayed narrow; stop-everything blocked the account |
| Per-account page wording (6 lines, owner/admin) | live | saved, rendered on the public page, cleared back to defaults |
| Blocked-address list + two CSVs | live | console tab |
| Isolation: own-domain allowance vs shared identity | live | 2000/day own domain, 100/day shared, transactional never capped |
| Reputation watchdog (0.30% complaints / 5% bounces) | live | pauses that account's bulk lane only |
| Domains screen, three lanes | live | inbox from verified domains; automation/calendar suggest subdomains; foreign host refused (400); clearing falls back |

### Two fleet facts learned the hard way here

1. **Anonymous arms belong on `PUBLIC_ROUTES`.** Both new open endpoints first landed on
   `API_KEY_ROUTES` and answered 401 "Authentication required" live while reading as open in the code.
   A unit test now fails if any `/public/` or `/unsubscribe` path sits on a credential list.
2. **The customer console has NO publisher.** `scripts/publish-admin-consoles.sh` mirrors
   `www-admin/` → `nginx/www-admin/<slug>/` only. `www-app/<slug>/index.html` is served from
   `nginx/www-app/<slug>/` and nothing in the fleet copies repo → served for it, so a committed console
   change stays invisible until someone copies it by hand (done here). Until a publisher exists, any
   change to `www-app/coreswift/index.html` must be followed by that copy, and the repo/served pair
   re-checked.
