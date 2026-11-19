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

## Open item to settle BEFORE building on the branch

`feat/ticketing-suite` is **16 commits ahead of `main` and 1 behind**. Real shipped work (admin tickets separating
automated reports from customer tickets, vendor promotional mailboxes, the profile close-account control) exists on
that branch and not on main. Reconcile the two before building, or the suite will sit on code the live app does not
have. This is the same class of drift that produced the duplicate-migration outage on 17 Nov.
