-- 062_fix_worker_schema.sql
-- See notes in memory. Adds the per-entity health columns to account_health
-- and the Flawless-Follow-up columns to business_profiles that the background
-- workers reference but were missing because those two tables had been created
-- by a divergent manual schema (dashboard-style account_health, directory-style
-- business_profiles) instead of migrations 022/023.
-- Both tables were empty (0 rows) when applied on 2026-08-12.

DO $$ BEGIN
    CREATE TYPE business_unit AS ENUM ('agency', 'directory', 'saas');
EXCEPTION WHEN duplicate_object THEN NULL; END $$;

DO $$ BEGIN
    CREATE TYPE user_state AS ENUM ('lead_captured', 'pending_onboarding', 'active', 'inactive', 'churned');
EXCEPTION WHEN duplicate_object THEN NULL; END $$;

ALTER TABLE account_health
    ADD COLUMN IF NOT EXISTS score INTEGER DEFAULT 100,
    ADD COLUMN IF NOT EXISTS risk_level VARCHAR(20) DEFAULT 'healthy',
    ADD COLUMN IF NOT EXISTS last_active_at TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS signals JSONB DEFAULT '[]'::jsonb,
    ADD COLUMN IF NOT EXISTS last_intervention_at TIMESTAMPTZ;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'account_health_tenant_entity_key'
          AND conrelid = 'account_health'::regclass
    ) THEN
        ALTER TABLE account_health
            ADD CONSTRAINT account_health_tenant_entity_key
            UNIQUE (tenant_id, entity_type, entity_id);
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS idx_account_health_entity
    ON account_health (entity_type, entity_id);

ALTER TABLE business_profiles
    ADD COLUMN IF NOT EXISTS business_name VARCHAR(255),
    ADD COLUMN IF NOT EXISTS unit business_unit,
    ADD COLUMN IF NOT EXISTS current_state user_state DEFAULT 'lead_captured',
    ADD COLUMN IF NOT EXISTS subscription_active BOOLEAN DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS last_activity_at TIMESTAMPTZ DEFAULT CURRENT_TIMESTAMP;

UPDATE business_profiles
   SET business_name = name
 WHERE business_name IS NULL AND name IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_bp_unit_state_activity
    ON business_profiles (unit, current_state, last_activity_at)
    WHERE subscription_active = FALSE AND current_state IN ('pending_onboarding', 'active');
