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

CoreSwift's own `affiliates` module (`GET /api/affiliates/products`, `/api/affiliates/profile`, `/api/affiliates/referrals`) is a **separate, tenant-local** programme kept in this app's database — it is not FunnelSwift's commissionable catalogue and it does not sync to it.

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
| `affiliates` | Affiliate system | affiliates | Referral tracking and payouts |
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
Toggle them per plan in the admin console. The one tolerance left: a tenant with no active
plan row at all keeps its modules.
