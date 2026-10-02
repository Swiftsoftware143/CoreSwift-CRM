# CoreSwift CRM — Admin Guide

## System Overview

CoreSwift CRM is the central CRM and automation platform. It manages contacts, deals, pipelines, campaigns, and email sequences with full template-based email delivery.

## Quick Reference

- **Backend:** Rust (Axum) @ port 8084, systemd unit `coreswift-crm`
- **Database:** PostgreSQL (docker: swift-postgres-1) — `coreswift` database
- **Admin Web App:** `/var/www/coreswiftcrm/` served by nginx
- **Repo:** `/opt/swift/coreswift/`

## Email Templates (New)

All transactional emails use database-stored templates in the `email_templates` table. Templates support `{{variable}}` placeholders for dynamic content.

### Template Types

| Type | When Used | Available Merge Fields |
|---|---|---|
| `welcome` | Account creation | `{{name}}`, `{{email}}`, `{{password}}`, `{{app_url}}` |
| `purchase_confirmed` | Successful payment | `{{name}}`, `{{plan_name}}`, `{{app_url}}` |
| `password_reset` | Password reset request | `{{name}}`, `{{token}}`, `{{app_url}}` |

### API Endpoints

| Method | Path | Description |
|---|---|---|
| GET | `/api/email-templates` | List all templates (with pagination + template_type filter) |
| POST | `/api/email-templates` | Create a new template |
| GET | `/api/email-templates/:id` | Get a single template |
| PUT | `/api/email-templates/:id` | Update a template (partial fields) |
| DELETE | `/api/email-templates/:id` | Delete a template |
| GET | `/api/email-templates/merge-fields` | List available merge fields by type |

### Template Fields

- **name** — human-readable label (e.g. "Welcome Email")
- **template_type** — one of `welcome`, `purchase_confirmed`, `password_reset`
- **subject** — email subject line (supports `{{variable}}` interpolation)
- **body** — plain text body (supports `{{variable}}` interpolation)
- **html_body** — HTML body (supports `{{variable}}` interpolation)
- **is_html** — if true, uses `html_body`; otherwise plain `body`
- **is_default** — if true, this template serves as the fallback for its type

### How It Works

1. When a flow triggers (e.g. forgot password, registration, billing), it calls `send_template_email()` with the template type and variable map
2. The system looks up a matching DB template — tenant-specific first, then fallback to `is_default = true`
3. If no DB template exists, a hardcoded inline template is used
4. The rendered email is queued to `outbound_messages` for async delivery
5. A background worker picks up queued messages and sends via SMTP

### Admin UI

The admin interface includes a dedicated Email Templates page with:
- List view showing all templates with type badges
- Modal editor with subject, body, HTML body fields
- Merge field menu button to insert `{{variable}}` placeholders
- HTML/TEXT toggle between body modes
- Create / Edit / Delete actions
- Type filter to find specific templates

### Default Templates (Seeded)

Three default templates are seeded on first migration:
- **Welcome Email** — sent on account creation (includes credentials, next steps)
- **Purchase Confirmation** — sent on successful payment receipt
- **Password Reset** — sent with reset token and link

## MultiDirectory Integration (Booking Slots)

CoreSwift powers the **Booking CTA slot** on MultiDirectory business listing pages.

**Flow:**
1. Business owner enables Booking integration in their MultiDirectory dashboard (Integrations tab)
2. They select a CTA label from a controlled dropdown: "Book Appointment", "Schedule a Consultation", "Book Now", "Reserve a Table", "Claim Your Slot", "Book a Tour"
3. The CTA button renders on the business's public listing page
4. Clicking opens CoreSwift's booking widget/modal (inline date picker + time slot selection)
5. Booking data flows into CoreSwift's contact and deal tracking pipelines

**Controlled vocabulary only** — business owners cannot type custom CTA text. This keeps directory branding consistent.

## Route Mapping

| Frontend Page | Route | Methods |
|---|---|---|
| Dashboard | `/api/dashboard/stats` | GET |
| Contacts | `/api/contacts` | GET, POST |
| Companies | `/api/companies` | GET, POST |
| Deals | `/api/pipelines/:pipeline_id/opportunities` | GET, POST |
| Campaigns | `/api/campaigns` | GET, POST |
| Email Templates | `/api/email-templates` | GET, POST |
| Message Templates | `/api/comms/templates` | GET, POST |
| Plans | `/api/billing/plans` | GET, POST |
| Account plan (operator) | `/api/admin/tenants/:id/plan` | PUT |
| Audit | `/api/audit` | GET |

## Monitoring & Logs

- Service logs: `journalctl -u coreswift-crm -n 100 --no-pager`
- Health check: `curl http://localhost:8084/api/health`
- Database: `docker exec -it swift-postgres-1 psql -U swift -d coreswift`

## Affiliate Products & Commissions

CoreSwift does **not** push its plans to FunnelSwift. The commissionable product catalogue lives in FunnelSwift's `affiliate_products` table and is owned there — FunnelSwift's own plan-derived products, plus one platform-wide entry per sibling service (for example `CoreSwift Free`, `source_app = 'coreswift'`), each with its own price and commission rate. Nothing in this app writes to that table, so creating, renaming or deleting a plan here does not change the affiliate portal's catalogue. Products are managed in the FunnelSwift admin (**Affiliate Products**), which is the source of truth for what an affiliate can promote.

What CoreSwift *does* report is the commission trigger: when a tenant's subscription moves onto a **paid** plan, `src/billing/handlers.rs` posts the account owner's email, the plan name and its price to FunnelSwift, fire-and-forget inside a `tokio::spawn` so billing is never delayed:

| Event | Call |
|-------|------|
| Tenant moves to a paid plan | `POST {FUNNELSWIFT_URL}/api/v1/internal/affiliate/upgrade-event`, with the shared `x-internal-key` header |

FunnelSwift matches that email back to the affiliate lead, credits the commission against the product whose `source_app` is `coreswift`, and ignores a repeat of the same event id, so nothing is credited twice. Attribution is by the email address the referred business signed up with, and only a move onto a **paid** plan counts. See "Conversion Tracking" in the user guide for the full chain.

**Environment variables:** `FUNNELSWIFT_URL` (default `http://localhost:8080`) and `INTERNAL_SYNC_KEY` — the shared `x-internal-key` value the receiver checks. If `FUNNELSWIFT_URL` is empty, or the tenant has no owner email, the call is skipped and the rest of the app is unaffected.

CoreSwift carries **no** affiliate system of its own (kanban t_3d81b041): the duplicate `/api/affiliates/*` routes, the CRM-side profile / payout / product-board code, the plan flag that gated them and the hub actions that exposed them to integrator webhook tokens were all retired, so the FunnelSwift programme above is the only affiliate system in the fleet. The now-empty schema (`affiliates`, `referrals`, `commission_payouts`, `affiliate_products`, `affiliate_product_selections`) is deliberately left in place — nothing in the app reads or writes it.

## Private Email — Admin Controls

### Overview

As `agency_admin` or `owner`, you can override email limits per tenant beyond what their plan allows. This is useful for giving specific tenants extra domains, more mailboxes, or higher alias limits without changing their plan.

### Tenant Email Limits API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/v1/private-email/admin/limits` | GET | List all tenants that have custom email limit overrides |
| `/api/v1/private-email/admin/limits/:tenant_id` | GET | View a tenant's plan defaults, your overrides, and effective limits |
| `/api/v1/private-email/admin/limits/:tenant_id` | PATCH | Set custom email limits for a tenant (upsert — creates or updates) |

**All admin routes require `agency_admin` or `owner` role.** Requests with other roles receive a 403 Forbidden.

### Setting Limits (PATCH `/api/v1/private-email/admin/limits/:tenant_id`)

Send a JSON body with any combination of fields you want to override:

```json
{
  "max_domains": 5,
  "max_mailboxes": 50,
  "max_aliases_per_mailbox": null
}
```

**Rules:**
- Set a number to override the plan default with your value
- Set `null` (or omit the field entirely) to fall back to what the tenant's plan provides
- All three fields are optional — only send what you want to change
- Values are upserted: if an override row doesn't exist for the tenant, one is created; if it does, the specified fields are updated while unspecified fields keep their current values

### Reading Limits (GET `/api/v1/private-email/admin/limits/:tenant_id`)

Example response:

```json
{
  "tenant_id": "550e8400-e29b-41d4-a716-446655440000",
  "plan_defaults": {
    "private_email": true,
    "max_domains": 3,
    "max_mailboxes": 10,
    "max_aliases_per_mailbox": 5,
    "catch_all_enabled": false
  },
  "overrides": {
    "id": "660e8400-e29b-41d4-a716-446655440001",
    "tenant_id": "550e8400-e29b-41d4-a716-446655440000",
    "max_domains": null,
    "max_mailboxes": 20,
    "max_aliases_per_mailbox": null,
    "created_at": "2026-07-01T00:00:00Z",
    "updated_at": "2026-07-15T00:00:00Z"
  },
  "effective_max_domains": 3,
  "effective_max_mailboxes": 20,
  "effective_max_aliases_per_mailbox": 5
}
```

**Field meanings:**
- `plan_defaults` — what the tenant's purchased plan provides (from the `plans.features` JSON column)
- `overrides` — the full override row you've set for this tenant (null if no overrides exist)
- `effective_*` — what's actually enforced: your override wins if set, otherwise the plan default

### Listing All Overrides (GET `/api/v1/private-email/admin/limits`)

Returns an array of all `tenant_email_limits` rows, sorted by newest first. Only returns tenants that have at least one override set.

### How Enforcement Works

When a tenant tries to add a domain or create a mailbox, the system checks in this order:

1. **Admin override first** — looks up the `tenant_email_limits` table for a row matching the tenant
2. **Plan defaults fallback** — if no override exists for a given field (or it's `null`), the tenant's plan features JSON is used
3. **Hard block on limit** — if the tenant has hit the effective limit, they receive a clear error message:
   - Domain limit: `"Domain limit reached (X/Y)"`
   - Mailbox limit: `"Mailbox limit reached (X/Y)"`
4. **Feature disabled** — if `private_email: false` in the plan features, the tenant gets `"Private Email not available on your plan"` for any operation

### Removing Overrides

To remove an override and let a tenant revert to their plan defaults, PATCH with `null` for all fields:

```json
{
  "max_domains": null,
  "max_mailboxes": null,
  "max_aliases_per_mailbox": null
}
```

The override row remains but all fields are null, meaning the plan defaults apply for every limit.

## System Email (Platform Mail Transport)

Where: **Admin console → Platform Email → System Email** (`admin.coreswiftcrm.com`).

This is the mail transport CoreSwift itself sends through — the provider and credential used by every
workspace that has NOT configured its own sending identity. Until this panel existed the credential
lived only in the container environment (`EMAIL_API_URL` / `EMAIL_API_KEY` / `EMAIL_FROM`), so it could
not be seen or rotated without a shell edit of `/etc/swift/env/coreswift.env` plus a container
recreate. It is now stored in the `admin_settings` row keyed `email` and edited from the panel.

### What the panel shows

- **Status** — configured, or not configured at all.
- **Carried by** — `database` (the credential saved here), `environment` (the server variables), or
  `database+environment` (the row is filled in part and the environment completes it).
- **Sending domain / From** — what a receiver will see.
- **Server environment** — whether `EMAIL_API_URL` and `EMAIL_API_KEY` are present (presence only,
  never values).

### What the panel edits

| Field | Meaning |
|---|---|
| Provider | The platform transport CoreSwift carries (`mailgun`) |
| Send endpoint (api_url) | Full send endpoint, e.g. `https://api.mailgun.net/v3/mg.example.com/messages`. Empty = use `EMAIL_API_URL` |
| From name | Display name on the message |
| From address | Must be an address on the sending domain, or receivers reject the message |
| API key | The provider credential. Leave the dots to keep the stored key; clear the field to remove it |

Buttons: **Save credential**, **Send test email** (a real send, recorded in the Communications Log
with the provider's own answer), **Use server environment** (removes the saved credential so the
`EMAIL_*` variables carry mail again).

### Resolution order

1. the field in the `admin_settings` row keyed `email`;
2. the `EMAIL_*` variable for **that same field**;
3. nothing — which is reported as *not configured*.

The order is per field, so a half-filled row still sends (a rotated key with no endpoint of its own
rides on `EMAIL_API_URL`), and an empty row is not "broken" — it means the environment carries mail.

### API

| Method | Route | Purpose |
|---|---|---|
| GET | `/api/admin/email-config` | Masked transport: status, which store carries it, key length — never the key |
| PUT | `/api/admin/email-config` | Save provider / api_url / api_key / from_address / from_name |
| DELETE | `/api/admin/email-config` | Remove the saved credential, return to `EMAIL_*` |
| POST | `/api/admin/email-config/test` | Send one real message through the platform transport |

All four are platform-admin only. The credential is sealed at rest with the app's `enc:v1:` envelope
(the same one `provider_keys` uses, under a scope of its own), so a database dump yields no usable
key, and no read route returns the key or a digest of it.

## Deployment

```bash
cd /opt/swift/coreswift
export CARGO_BUILD_JOBS=1
cargo build --release
systemctl restart coreswift-crm
```

## Plans & Feature Access

Every feature the admin can switch on/off per plan (source of truth: the module registry in the database — `modules` / `module_features`, the switches `GET /api/admin/modules` renders):

| Flag | Feature | Module | Notes |
|---|---|---|---|
| `campaigns` | Campaigns | campaigns | Sequenced email campaigns |
| `automation` | Automations | automation | Trigger/action engine |
| `checklists` | Checklists | checklists | Onboarding/process checklists |
| `ai_enabled` | AI scoring & helpers | ai_enabled | Lead scoring and AI helpers |
| `tickets` | Support tickets | tickets | In-house ticketing + email-to-ticket |
| `native_apps` | Native app connectors | native_apps | FunnelSwift, ADASwift, MissedCall, WorkflowSwift, CheatLayer, Multi-Directory |
| `telnyx` | SMS & voice (Telnyx) | telnyx | SMS, number management, call tracking |
| `round_robin` | Round-robin routing | round_robin | Fair lead distribution across a team |
| `events` | Event system | events | Internal event bus |
| `monitoring` | Monitoring & health | monitoring | Account health scoring and thresholds |
| `bookings` | Bookings & scheduling | bookings | Calendar booking pages |
| `google_calendar` | Google Calendar sync | google_calendar | Two-way calendar sync |
| `api_access` | API access | api_access | Per-tenant API keys |
| `webhooks` | Webhooks | webhooks | Outbound webhook delivery |
| `integrations` | Integrations | integrations | n8n and third-party integrations |
| `provider_keys` | Provider keys | provider_keys | Bring-your-own provider credentials |
| `support_widgets` | Support widgets | support_widgets | Embeddable support surfaces |
| `tracked_links` | Tracked links | tracked_links | Click tracking links |
| `private_email` | Private mailbox | private_email | Own domain + mailboxes (also has its own limits) |
| `portfolio` | Portfolio sync | portfolio | Cross-tenant portfolio management |

Behaviour: an explicit per-tenant entry in `tenant_plans.feature_overrides` wins, then the
tenant's **active plan's** assigned modules (`plan_modules` / `plan_module_features`). A flag
that resolves to nothing is **denied** — the module returns **402 Payment Required** — so a
module can no longer ship ungated and a renamed key can no longer silently stop enforcing.
Toggle them per plan in the admin console. The one tolerance left — and the console now says it out
loud instead of leaving it to be inferred: **a workspace with no plan row at all is granted EVERY
module** (the resolver answers `source = no_plan` and the gate lets it through), and none of its
numeric ceilings apply. It is not on the free tier; it is on the unlimited one. The account card in
**Modules & Plans → Per-tenant plan & overrides** prints `no plan row — all modules allowed` with the
denied list empty, and **Assign plan** on that same card (or `PUT /api/admin/tenants/:id/plan`) ends
the state — one press, and that press is the only way it ends. Measured 2026-10-02: 14 of the 17
workspaces on this deployment were in it, and all of them are operator-owned or minted by an ingest
arm, never a signup (a signup always writes a `free` row).

### Putting an account on a plan (operator action)

Assigning a plan is an **operator** action, not a tenant self-service one. `POST` / `PATCH
/api/billing/subscription` are both platform-gated **and both take the tenant from the caller's own
token**, so they can only ever write the operator's own workspace — and `POST` additionally answers
**409** for any account that already holds a row, which every signup writes. The instrument that
acts on a **target** account is:

| Route | Method | Who | What it does |
|---|---|---|---|
| `/api/admin/tenants/:id/plan` | `PUT` | platform admin only | Puts the account in the path on a plan, optionally with a billing cycle |

Body — `plan_slug` (preferred: it is unique in `plans`) or `plan_id`, plus an optional
`billing_cycle` of `monthly` or `yearly`. Omitting the cycle keeps the account's current one, and a
first-time assignment defaults to `monthly`.

```json
{ "plan_slug": "starter", "billing_cycle": "monthly" }
```

It works whether or not the account already has a `tenant_plans` row: an existing row is updated,
and an account with **no** row gets one created — `created_row` in the answer says which happened.
Most accounts have no row, because only a signup seat writes one. The answer echoes the plan and the
cycle the server actually wrote, and every call is recorded in the audit log as
`subscription.plan_assigned`, naming the operator.

In the admin console: **Modules & Plans → Per-tenant plan & overrides** — pick the account, pick the
plan and the cycle, press **Assign plan**. The line under the button prints what the server wrote and
whether the row was created or updated, and the entitlements underneath are re-read.

| Answer | When |
|---|---|
| **200** | written; `created_row` is `true` when the account had no plan row before |
| **403** | the caller is not a platform admin — a tenant `owner`, or a role string such as `agency_admin` whose `users.is_platform_admin` is false |
| **404** | no account with that id |
| **422** | no such active plan, or a `billing_cycle` outside the schema's vocabulary |

What it deliberately does **not** do: it decides no pricing and no plan contents (no `plans`,
`plan_modules` or `plan_module_features` row is touched), it never writes
`tenant_plans.feature_overrides` — the per-account entitlement instrument stays the platform-gated
`POST /api/admin/tenants/:id/overrides` — and it never writes credits. A paid assignment posts the
same affiliate attribution as the subscription writers (see *Affiliate Products & Commissions*).

### Numeric limits (the `limits` module, plus the two on `private_email`)

The same matrix carries the numeric ceilings: one `module_features` row of kind `limit` per quota,
rendered as a **number box** instead of a tick. Clearing the box switches that limit OFF for the plan
— the gate reads that as **0**, i.e. "not available on your plan", and denies with **402**. A
negative number is the unlimited sentinel. An enabled limit with no number has no ceiling. The top
tier (the most expensive active row) grants every one of them.

| Feature key | Module | Ceiling | Read by |
|---|---|---|---|
| `limit_max_widgets` | limits | support widgets per account | `POST /api/widgets` |
| `limit_max_industries` | limits | industry dashboards | `POST /api/industries` |
| `limit_max_contacts` | limits | contacts | `POST /api/contacts`, `POST /api/csv/import/contacts` — the paths where the **workspace's own user** adds a contact. A lead **another app** captured is never refused, on any arm including the external api-key surface (see *Capture over the ceiling* below) |
| `limit_max_users` | limits | active users (`users.is_active`) | `POST /api/auth/register` (accepting an invite) |
| `limit_pipelines` | limits | pipelines | `POST /api/pipelines` |
| `limit_integrations` | limits | connected integrations | `POST /api/integrations` |
| `limit_api_calls_per_day` | limits | authenticated api-key requests per **UTC day** (`GET /api/external/lists`, `POST /api/external/contacts`) | `external_api::resolve_key` — every request that resolves a personal API key |
| `email_domains` | private_email | own sending domains | adding a Private Email domain |
| `email_mailboxes` | private_email | mailboxes | provisioning a mailbox |

A ceiling is checked **only on the route that adds the counted row** — listing, editing and deleting
keep working at the ceiling, so a workspace can always free a slot. The number a customer sees in
`GET /api/auth/me/usage` is the same `COUNT(*)` the gate compares. Note two consequences of the
authored numbers: on **free** a workspace sells **0 connected integrations** (`limit_integrations`)
and **1 active user**, so connecting an integration — or inviting a second member — answers
**402** until the plan changes. Raise those numbers here if that is not the intent.

One ceiling counts a **flow** rather than a collection of rows: `limit_api_calls_per_day` meters the
api-key surface. The count lives in the `api_call_usage` table, one row per workspace per **UTC**
day — the day boundary is the primary key, so the quota resets by itself at 00:00 UTC and there is
no job to run. It is incremented on `GET /api/external/lists` and `POST /api/external/contacts` —
the two endpoints the Integration Centre hands to a spoke app — and **only** there: the console's
own routes are never counted, so a workspace that reaches its daily ceiling keeps full use of the
app and only its API answers **402** until the next UTC day. Both endpoints are metered, so a
client that re-pushes the same lead (which costs no contact) still consumes calls. The number
`GET /api/auth/me/usage` renders as `api_calls_today` is the same row the ceiling is compared
against. A refused call is not counted, and a workspace with no active plan row is not capped.

### Capture over the ceiling — why a lead from another app is never refused

`limit_max_contacts` is a hard stop **only** where the workspace's own user adds a contact: *Add
Contact* in the console and the CSV import. The seven **ingest** arms that accept a lead **another
app has already captured** are deliberately **not** gated:

| Ingest arm | Who feeds it |
|---|---|
| `POST /api/external/contacts` | the **spoke ingest** — the endpoint the Integration Centre hands a sibling app. FunnelSwift (`spawn_lead_push`), ADASwift, WorkflowSwift, IncentiveSwift, MissedCall Respondr and Multi-Directory push the leads they captured here (personal API key) |
| `POST /api/internal/contacts` | Multi-Directory / ZaarHub lead sync (internal key) |
| `POST /api/v1/webhooks/cross-app/tag-sync` | FunnelSwift lead + tag sync (internal key) |
| `POST /api/v1/internal/tag-provision` | FunnelSwift tag provisioning (internal key) — it mints its own workspace per call, so it can never be over a ceiling |
| `POST /inbound/v3/{key_prefix}/contact-sync` | satellite apps pushing surveyed contacts (satellite key) |
| `POST /api/v1/webhooks/mailgun/inbound` | an inbound email to a Private Email mailbox |
| `POST /api/webhook/{token}/contacts.create` | a workspace's own configured webhook (Zapier / n8n) |

The reason is a measurement, not a preference: those callers are **fire-and-forget**. FunnelSwift's
push into the hub is a `tokio::spawn` whose only failure handling is a `warn!` log line
(`src/coreswift.rs`, `spawn_lead_push`) — there is no retry and no queue — so a **402** there does not
upsell. It **silently drops a lead the sibling already captured**, and because the caller never reads
the response, nobody ever sees the refusal.

The external api-key surface (`POST /api/external/contacts`) was listed here as a tenant-facing add
until 2026-10-02 (kanban `t_e2364c41`). Measuring its callers — the six sibling spokes above, with
the fire-and-forget push — and reading this guide's own description of it as *the endpoint the
Integration Centre hands to a spoke app* showed that classification was wrong: a key-holding
workspace could lose every satellite lead it captured, silently, the moment it reached its ceiling.
Its **new-row** arm now behaves like every other ingest arm. A re-delivered lead already resolved to
its row and still does, at any count.

What happens instead: the lead lands, the workspace's count goes **past** its ceiling, and the console
says so. The workspace dropdown prints `Usage: 101/100 contacts ⚠ over plan limit` and the Dashboard
shows an **Over your plan's contact limit** card naming the real numbers. `GET /api/auth/me/usage`
carries the two fields the notice is built from (`contacts_limit` and `contacts_over_limit`), so the
readout a tenant sees and the number the gate enforces can never disagree.

The ceiling still enforces: while the workspace is over, a contact the tenant adds in the console or
by CSV import is refused with **402** `Contact limit reached (n/m). Upgrade your plan for more
contacts.` Reading, editing and deleting contacts keeps working at every count, so a workspace can
always get back under the limit. (Measured on this deployment 2026-10-02: the three workspaces that
hold contacts — 122 / 81 / 78 rows — have no active plan row, so they are uncapped; the only
plan-attached workspaces are Free-plan probes. The ceiling is a live upsell signal on the add paths
and a visible advisory on captures.)

If you would rather the ingest arms refuse as well, that is a product change with a prerequisite:
every sibling app needs a retry queue **before** the refusal can be safe, or captured leads are lost
at the boundary with no error surfaced to anyone.

### Limits that are NOT in the matrix any more

Two of the eleven limit rows have no enforceable quantity behind them and are absent from the
matrix rather than left as a number that does nothing:

| Retired key | Why |
|---|---|
| `limit_storage_gb` | **CoreSwift stores no files, and this is permanent.** No `bytea`/blob column exists anywhere in the schema (the only one is the migration ledger's own checksum), there is no upload or attach endpoint, and the only per-workspace content is CRM rows — measured at **210 bytes per contact** on this box. At each tier's OWN contact ceiling that is ~0.02% of the GB ceiling it was priced against (agency: 50,000 contacts ≈ 10.5 MB against 50 GB), and the whole database — every workspace, 106 tables, indexes and all — is 22 MB, below even the free tier's 0.1 GB. A size source could not fire before the contact cap did, so the key stays retired. `account_health.storage_mb` is declared and written by no code path: do not wire it. |
| `limit_monthly_credits` | duplicate of `plans.monthly_credits`, which the credit engine reads |

`limit_api_calls_per_day` was retired with them in kanban `t_f49e4299` and **re-registered** once
its counter existed (see the table above). The authored numbers survive in plan data
(`plans.features` carries `storage_gb` / `api_calls_per_day`; `plans.monthly_credits` is untouched),
so a retired key comes back the same way once the quantity it counts is measurable.
