# CoreSwift (204) Schema Alignment Report

## Summary
Successfully aligned the 204 Rust codebase with the existing PostgreSQL database. All major endpoints are now functional.

## Database Schema Changes

### Views Created (table name mismatches)
1. **`scores`** → aliases `contact_scores` table (code queries `scores` table)
2. **`stage_history`** → aliases `opportunity_stage_history` table (code queries `stage_history`)

### Columns Added (ALTER TABLE ADD COLUMN IF NOT EXISTS)
| Table | Columns Added |
|-------|--------------|
| contacts | `title`, `company_id`, `gender`, `address_line1`, `address_line2`, `postal_code`, `notes` |
| pipeline_stages | `position`, `is_won_stage`, `is_lost_stage`, `description` |
| companies | `domain`, `address_line1`, `address_line2`, `postal_code`, `notes` |
| tags | `domain`, `is_active` |
| users | `avatar_url`, `phone`, `first_name`, `last_name` |
| lists | `active_member_count` |

### Data Migrations (column renames → new columns)
- `contacts.job_title` → `contacts.title` (copied data)
- `pipeline_stages.sort_order` → `pipeline_stages.position` (copied data)
- `pipeline_stages.is_won` → `pipeline_stages.is_won_stage` (copied data)
- `pipeline_stages.is_lost` → `pipeline_stages.is_lost_stage` (copied data)
- `companies.website` → `companies.domain` (copied data)
- `lists.dynamic_rules` → `lists.rules` (copied data)

### Type Conversions
- `opportunities.value`: changed from `NUMERIC(15,2)` to `DOUBLE PRECISION` to match Rust `Option<f64>`

### SQL Query Fixes (code changes)
| File | Issue | Fix |
|------|-------|-----|
| `src/webhook/actions.rs:118` | `SELECT ... stages FROM pipelines` — no `stages` column | Replaced with JSON subquery joining `pipeline_stages` |
| `src/webhook/actions.rs:896` | `SELECT ... plan, plan_id FROM tenants` — no such columns | Changed to `NULL::text AS plan, NULL::uuid AS plan_id` |
| `src/webhook/actions.rs:989,995` | `FROM audit_log` (singular) — table is `audit_logs` | Changed to `FROM audit_logs` |
| `src/ai/handlers.rs:120` | `SELECT name FROM contacts` — no `name` column | Changed to `SELECT CONCAT(first_name, ' ', last_name) AS name` |
| `src/admin_actions/handlers.rs:964` | `SELECT email FROM tenants` — no `email` column | Changed to subquery: `(SELECT u.email FROM users u WHERE u.tenant_id = t.id AND u.role = 'owner' LIMIT 1) AS email` |
| `src/lists/handlers.rs:19` | `$5::list_type` cast — no `list_type` enum type in DB | Removed `::list_type` cast |

## Working Endpoints
- `GET /api/health`
- `POST /api/auth/register`
- `POST /api/auth/login`
- `POST/GET /api/contacts`
- `PATCH /api/contacts/:id`
- `POST/GET /api/companies`
- `POST/GET /api/pipelines`
- `POST /api/pipelines/:id/stages`
- `POST/GET /api/pipelines/:id/opportunities`
- `POST/GET /api/tags`
- `POST/GET /api/lists`
- `POST /api/lists/:id/members`
- `POST/GET /api/scoring/rules`
- `GET /api/dashboard/stats`
- `GET /api/audit/`
- `POST /api/webhook/:token/:action`

## Build & Deploy
- Binary name is `crm-swift` (from Cargo.toml package name)
- Symlink created: `target/release/coreswift-api → crm-swift`
- Service file uses `ExecStart=/opt/swift/coreswift/target/release/coreswift-api`

## Disk Space
- Freed ~9GB by cleaning `/tmp` build artifacts and stale Rust target directories
- Current usage: ~80% of 38GB

## Known Issues/Limitations
1. **No existing data migration** for the new columns (all NULL on existing records)
2. **Service restart** clears prepared statement cache (had one `cached plan must not change result type` error after ALTER TABLE)
3. **Webhook endpoint** is at `/api/webhook/:token/:action` (POST only), not a simple CRUD API at `/api/webhooks`
