-- 023_create_business_profiles.sql
-- Core database schema matching the "Flawless Follow-up" system design
-- Bridges CRM Swift tables with Steve Rosenberg's business_profiles concept
-- and Supabase-ready SQL for the Flawless Follow-up automated system.

-- Channel type enum (must exist before followup_queue references it)
DO $$ BEGIN
    CREATE TYPE channel_type AS ENUM ('sms', 'email', 'hybrid');
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

-- 1. ENUMS FOR SYSTEM STATES
DO $$ BEGIN
    CREATE TYPE business_unit AS ENUM ('agency', 'directory', 'saas');
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

DO $$ BEGIN
    CREATE TYPE user_state AS ENUM ('lead_captured', 'pending_onboarding', 'active', 'inactive', 'churned');
EXCEPTION
    WHEN duplicate_object THEN NULL;
END $$;

-- 2. CENTRAL USERS TABLE (already exists in migration 002)
-- Add David's schema fields if not present (idempotent)
ALTER TABLE users ADD COLUMN IF NOT EXISTS phone VARCHAR(50);
ALTER TABLE users ADD COLUMN IF NOT EXISTS first_name VARCHAR(100);
ALTER TABLE users ADD COLUMN IF NOT EXISTS last_name VARCHAR(100);

-- 3. BUSINESS PROFILES TABLE
-- Tracks specific business settings/tiers across agency, directory, and SaaS
CREATE TABLE IF NOT EXISTS business_profiles (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    business_name VARCHAR(255) NOT NULL,
    unit business_unit NOT NULL,
    current_state user_state DEFAULT 'lead_captured',
    stripe_customer_id VARCHAR(255),
    subscription_active BOOLEAN DEFAULT FALSE,
    last_activity_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

-- Performance indexes for the If-Not-Then background queries
CREATE INDEX IF NOT EXISTS idx_bp_unit_state_activity
    ON business_profiles (unit, current_state, last_activity_at);

CREATE INDEX IF NOT EXISTS idx_business_profiles_user ON business_profiles(user_id);
CREATE INDEX IF NOT EXISTS idx_business_profiles_activity ON business_profiles(last_activity_at)
    WHERE subscription_active = FALSE AND current_state IN ('pending_onboarding', 'active');

-- 4. EVENT LOGS TABLE - The Air Traffic Controller
-- Every button click, download, or page view dumps an entry here.
CREATE TABLE IF NOT EXISTS event_logs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    business_profile_id UUID REFERENCES business_profiles(id) ON DELETE CASCADE,
    event_name VARCHAR(100) NOT NULL,
    metadata JSONB,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_event_logs_profile_created ON event_logs (business_profile_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_event_logs_event ON event_logs(event_name);

-- 5. FOLLOW-UP QUEUE TABLE
-- Tracks scheduled and executed touches for Twilio/SendGrid automation.
-- A database webhook (Supabase or pg_notify) can fire an Edge Function
-- or the CRM Swift binary worker picks up rows directly.
CREATE TABLE IF NOT EXISTS followup_queue (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    business_profile_id UUID NOT NULL REFERENCES business_profiles(id) ON DELETE CASCADE,
    scheduled_for TIMESTAMP WITH TIME ZONE NOT NULL,
    channel channel_type NOT NULL,
    template_slug VARCHAR(100) NOT NULL,
    is_executed BOOLEAN DEFAULT FALSE,
    is_cancelled BOOLEAN DEFAULT FALSE,
    executed_at TIMESTAMP WITH TIME ZONE,
    created_at TIMESTAMP WITH TIME ZONE DEFAULT CURRENT_TIMESTAMP
);

-- Index for the automation engine looking for messages that need to fire right now
CREATE INDEX IF NOT EXISTS idx_fq_scheduled_unexecuted
    ON followup_queue (scheduled_for)
    WHERE is_executed = FALSE AND is_cancelled = FALSE;

CREATE INDEX IF NOT EXISTS idx_followup_queue_profile ON followup_queue(business_profile_id);
