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

## Route Mapping

| Frontend Page | Route | Methods |
|---|---|---|
| Dashboard | `/api/dashboard/stats` | GET |
| Contacts | `/api/contacts` | GET, POST |
| Companies | `/api/companies` | GET, POST |
| Deals | `/api/pipelines/deals` | GET, POST |
| Campaigns | `/api/campaigns` | GET, POST |
| Email Templates | `/api/email-templates` | GET, POST |
| Message Templates | `/api/comms/templates` | GET, POST |
| Plans | `/api/billing/plans` | GET, POST |
| Audit | `/api/audit` | GET |

## Monitoring & Logs

- Service logs: `journalctl -u coreswift-crm -n 100 --no-pager`
- Health check: `curl http://localhost:8084/api/health`
- Database: `docker exec -it swift-postgres-1 psql -U swift -d coreswift`

## Deployment

```bash
cd /opt/swift/coreswift
export CARGO_BUILD_JOBS=1
cargo build --release
systemctl restart coreswift-crm
```
