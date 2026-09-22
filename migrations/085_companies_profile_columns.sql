-- 085_companies_profile_columns.sql - the 5 company profile columns the API has always published.
--
-- t_c21489e7. `src/companies/handlers.rs` names `domain, address_line1, address_line2, postal_code,
-- notes` in its INSERT (line 39) AND in its UPDATE (line 78), and `Company`
-- (`src/companies/models.rs`) - the `sqlx::FromRow` type behind `SELECT *` / `RETURNING *` in all
-- five handlers - declares all five as fields. None of them existed in the live database, so EVERY
-- `/api/companies` route failed: the write side with `column "domain" of relation "companies" does
-- not exist`, the read side with `no column found for name: domain` (sqlx 0.8.6's derive is
-- name-based, see the fleet deploy notes).
--
-- These are additive and nullable, matching the neighbours' style, and every writer is satisfied by
-- them exactly as it is written: `CreateCompanyRequest` / `UpdateCompanyRequest` expose the five as
-- `Option<String>` and bind NULL when a caller omits them. Nothing to backfill - `companies` held 0
-- rows at the time this file was written (verified `select count(*) from companies`).
--
-- This is NOT "silence the audit". The surface is published and advertised: the served customer
-- guide (`public/guide.html`) promises "Track organizations with name, domain, industry, and
-- contact count", the tenant SPA's Companies tab calls `API.get('/companies')`, and the sibling
-- `contacts` table already carries exactly `address_line1, address_line2, postal_code, notes` -
-- this is the app's own shape. Dropping the fields instead would delete published API surface.
--
-- Migration 004 (which creates `companies`) is already applied and is deliberately not edited: it
-- never defined `account_id` either, which is the live drift that made the four scope filters in
-- this module dead, fixed in code rather than by adding a second tenant column.
--
-- No semicolon appears inside these comments - the runner executes the whole file.

ALTER TABLE companies ADD COLUMN IF NOT EXISTS domain        VARCHAR(255);
ALTER TABLE companies ADD COLUMN IF NOT EXISTS address_line1 VARCHAR(255);
ALTER TABLE companies ADD COLUMN IF NOT EXISTS address_line2 VARCHAR(255);
ALTER TABLE companies ADD COLUMN IF NOT EXISTS postal_code   VARCHAR(50);
ALTER TABLE companies ADD COLUMN IF NOT EXISTS notes         TEXT;

-- The list handler orders by name and the SPA searches on domain, so keep the lookup shape honest.
CREATE INDEX IF NOT EXISTS idx_companies_domain ON companies(tenant_id, domain);
