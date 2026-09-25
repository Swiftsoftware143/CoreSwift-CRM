-- 000_baseline_live_schema.sql
-- CoreSwift-CRM baseline: the shape this database ACTUALLY has.
--
-- WHY THIS FILE EXISTS (card t_d8b2e888, measured 2026-09-25 on HEAD 0474949)
--   `migrations/` is a delta history written against a database that was imported from an earlier
--   product lineage; it could not build its own schema.  Measured: on an EMPTY database the chain
--   already dies at 088 (`column "tenant_id" referenced in foreign key constraint does not exist`),
--   and of the tables it does create, 22 have a shape the live database has never had - 43 live
--   columns are missing (account_health 13, notification_queue 11, business_profiles 9, ...) while
--   3 columns live never had are invented.  So a fresh install / restored dump came up with a
--   schema no application code was written against, and none of 050..087 could be reproduced.
--
-- WHAT THIS FILE IS
--   The live catalog (columns, types, NOT NULLs, defaults, collations, primary/unique/check/
--   foreign-key constraints, indexes) of every table that `migrations/` already creates, in
--   foreign-key dependency order.  The 15 tables that have NO DDL anywhere in `migrations/` are
--   NOT here - they are created by 092_missing_ddl_for_15_live_tables.sql instead.
--
-- WHY VERSION 000
--   The file has to run BEFORE 001 (a table has to exist before a later file can ALTER it) and
--   every version 1..91 is taken, so 000 is the only slot left.  sqlx 0.8's Migrator::run applies
--   every migration that is not in the applied set, independent of order
--   (sqlx-core-0.8.6/src/migrate/migrator.rs:168-182) and validate_applied_migrations only rejects
--   an APPLIED version that has no file - this is the same mechanism 037/082 use in this repo.
--
-- ON THE LIVE DATABASE EVERY STATEMENT IS A NO-OP
--   CREATE TABLE IF NOT EXISTS / CREATE INDEX IF NOT EXISTS / guarded CREATE TYPE.  Verified by a
--   whole-catalog fingerprint of live before and after applying this file inside BEGIN ... ROLLBACK
--   (evidence: /opt/swift/audits/cs-missing-ddl-t_d8b2e888/).
--
-- NOT INCLUDED ON PURPOSE: `_migrations` (the ledger of the pre-sqlx lineage this database was
-- imported from - its filenames do not exist in this repo) and `_sqlx_migrations` (created by the
-- runner).  Neither is application schema.

-- The extensions this database uses: 001 declares them, but a table default can call them, so they
-- have to be here too (both statements are no-ops on live).
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- The three enums a live column uses that 023/062 create in guarded DO blocks (002 creates the
-- fourth, user_role, with a bare CREATE TYPE and no live column is of that type).
DO $$ BEGIN
    CREATE TYPE public.channel_type AS ENUM ('sms', 'email', 'hybrid');
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;
DO $$ BEGIN
    CREATE TYPE public.business_unit AS ENUM ('agency', 'directory', 'saas');
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;
DO $$ BEGIN
    CREATE TYPE public.user_state AS ENUM ('lead_captured', 'pending_onboarding', 'active', 'inactive', 'churned');
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;

CREATE TABLE IF NOT EXISTS public.app_admin_configs (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    app_slug character varying(100) NOT NULL,
    config jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT app_admin_configs_pkey PRIMARY KEY (id),
    CONSTRAINT app_admin_configs_app_slug_key UNIQUE (app_slug)
);

CREATE TABLE IF NOT EXISTS public.available_providers (
    key character varying(64) NOT NULL,
    name character varying(128) NOT NULL,
    description text,
    requires_base_url boolean DEFAULT false,
    requires_metadata jsonb DEFAULT '[]'::jsonb,
    icon character varying(32),
    CONSTRAINT available_providers_pkey PRIMARY KEY (key)
);

CREATE TABLE IF NOT EXISTS public.modules (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    key character varying(64) NOT NULL,
    name character varying(120) NOT NULL,
    description text,
    icon character varying(32),
    sort_order integer DEFAULT 0 NOT NULL,
    is_active boolean DEFAULT true NOT NULL,
    legacy_feature_key character varying(64),
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT modules_pkey PRIMARY KEY (id),
    CONSTRAINT modules_key_key UNIQUE (key)
);
CREATE INDEX IF NOT EXISTS idx_modules_key ON public.modules USING btree (key);

CREATE TABLE IF NOT EXISTS public.native_apps (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    slug character varying(100) NOT NULL,
    name character varying(255) NOT NULL,
    description text DEFAULT ''::text,
    auth_type character varying(20) NOT NULL,
    auth_config jsonb DEFAULT '{}'::jsonb NOT NULL,
    access_level character varying(20) NOT NULL,
    is_active boolean DEFAULT true,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT native_apps_pkey PRIMARY KEY (id),
    CONSTRAINT native_apps_slug_key UNIQUE (slug),
    CONSTRAINT native_apps_access_level_check CHECK (access_level = ANY (ARRAY['admin'::character varying, 'admin_tenant'::character varying])),
    CONSTRAINT native_apps_auth_type_check CHECK (auth_type = ANY (ARRAY['api_key'::character varying, 'oauth2'::character varying, 'basic'::character varying]))
);

CREATE TABLE IF NOT EXISTS public.plans (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    name character varying(100) NOT NULL,
    slug character varying(50) NOT NULL,
    description text,
    price_monthly numeric(10,2) DEFAULT 0 NOT NULL,
    price_yearly numeric(10,2) DEFAULT 0 NOT NULL,
    features jsonb DEFAULT '{}'::jsonb NOT NULL,
    checkout_url text,
    is_active boolean DEFAULT true NOT NULL,
    sort_order integer DEFAULT 0,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    monthly_credits integer DEFAULT 0,
    max_industries integer DEFAULT 1,
    payment_provider character varying(64),
    thank_you_url character varying(500),
    max_contacts integer DEFAULT 1000,
    CONSTRAINT plans_pkey PRIMARY KEY (id),
    CONSTRAINT plans_slug_key UNIQUE (slug)
);

CREATE TABLE IF NOT EXISTS public.telnyx_config (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    api_key text DEFAULT ''::text NOT NULL,
    profile_id character varying(255),
    messaging_profile_id character varying(255),
    webhook_secret character varying(255),
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT telnyx_config_pkey PRIMARY KEY (id)
);

CREATE TABLE IF NOT EXISTS public.tenants (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    name character varying(255) NOT NULL,
    slug character varying(100) NOT NULL,
    logo_url text,
    primary_color character varying(7) DEFAULT '#3B82F6'::character varying,
    accent_color character varying(7) DEFAULT '#10B981'::character varying,
    custom_domain character varying(255),
    branding_name character varying(255),
    settings jsonb DEFAULT '{}'::jsonb,
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    integration_config jsonb DEFAULT '{}'::jsonb,
    allowed_sources text[] DEFAULT '{}'::text[],
    plan_id uuid,
    is_portfolio boolean DEFAULT false,
    industry_slug character varying(100) DEFAULT 'site-flipping'::character varying,
    CONSTRAINT tenants_pkey PRIMARY KEY (id),
    CONSTRAINT tenants_custom_domain_key UNIQUE (custom_domain),
    CONSTRAINT tenants_slug_key UNIQUE (slug),
    CONSTRAINT tenants_plan_id_fkey FOREIGN KEY (plan_id) REFERENCES plans(id)
);
CREATE INDEX IF NOT EXISTS idx_tenants_custom_domain ON public.tenants USING btree (custom_domain);
CREATE INDEX IF NOT EXISTS idx_tenants_slug ON public.tenants USING btree (slug);

CREATE TABLE IF NOT EXISTS public.ticket_inboxes (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    name text NOT NULL,
    email_fwd text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT ticket_inboxes_pkey PRIMARY KEY (id),
    CONSTRAINT ticket_inboxes_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_ticket_inboxes_tenant ON public.ticket_inboxes USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.users (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    email character varying(255) NOT NULL,
    password_hash text NOT NULL,
    name character varying(255) NOT NULL,
    role text DEFAULT 'user'::text NOT NULL,
    avatar_url text,
    is_active boolean DEFAULT true NOT NULL,
    last_login_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    phone character varying(50),
    first_name character varying(100),
    last_name character varying(100),
    password_changed_at timestamp with time zone,
    is_platform_admin boolean DEFAULT false NOT NULL,
    CONSTRAINT users_pkey PRIMARY KEY (id),
    CONSTRAINT users_email_key UNIQUE (email),
    CONSTRAINT users_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_users_email ON public.users USING btree (email);
CREATE INDEX IF NOT EXISTS idx_users_is_platform_admin ON public.users USING btree (is_platform_admin) WHERE is_platform_admin;
CREATE INDEX IF NOT EXISTS idx_users_tenant ON public.users USING btree (tenant_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_users_tenant_email ON public.users USING btree (tenant_id, email);
CREATE INDEX IF NOT EXISTS idx_users_tenant_id ON public.users USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.webhook_endpoints (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    name character varying(255) NOT NULL,
    url text NOT NULL,
    secret character varying(255),
    events text[] DEFAULT '{}'::text[] NOT NULL,
    retry_count integer DEFAULT 3,
    timeout_ms integer DEFAULT 5000,
    is_active boolean DEFAULT true NOT NULL,
    last_triggered_at timestamp with time zone,
    failure_count integer DEFAULT 0,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT webhook_endpoints_pkey PRIMARY KEY (id),
    CONSTRAINT webhook_endpoints_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_webhook_endpoints_events ON public.webhook_endpoints USING gin (events);
CREATE INDEX IF NOT EXISTS idx_webhook_endpoints_tenant ON public.webhook_endpoints USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.account_health (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid,
    tenant_name text,
    status text DEFAULT 'healthy'::text,
    last_login timestamp with time zone,
    signup_date timestamp with time zone,
    days_inactive integer DEFAULT 0,
    total_users integer DEFAULT 0,
    total_prospects integer DEFAULT 0,
    total_deals integer DEFAULT 0,
    storage_mb real DEFAULT 0,
    api_calls_24h integer DEFAULT 0,
    issues text[],
    health_score integer DEFAULT 100,
    checked_at timestamp with time zone DEFAULT now(),
    entity_type character varying(50) DEFAULT 'tenant'::character varying,
    entity_id uuid,
    updated_at timestamp with time zone DEFAULT now(),
    score integer DEFAULT 100,
    risk_level character varying(20) DEFAULT 'healthy'::character varying,
    last_active_at timestamp with time zone,
    signals jsonb DEFAULT '[]'::jsonb,
    last_intervention_at timestamp with time zone,
    CONSTRAINT account_health_pkey PRIMARY KEY (id),
    CONSTRAINT account_health_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_account_health_entity ON public.account_health USING btree (entity_type, entity_id);

CREATE TABLE IF NOT EXISTS public.ada_campaign_triggers (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    name character varying(255) NOT NULL,
    trigger_on character varying(50) NOT NULL,
    ada_campaign_id character varying(255) NOT NULL,
    schedule_delay_minutes integer DEFAULT 0,
    active boolean DEFAULT true,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT ada_campaign_triggers_pkey PRIMARY KEY (id),
    CONSTRAINT ada_campaign_triggers_trigger_on_check CHECK (trigger_on = ANY (ARRAY['user_created'::character varying, 'contact_created'::character varying, 'account_activated'::character varying, 'scan_complete'::character varying, 'referral_confirmed'::character varying, 'commission_earned'::character varying, 'payout_processed'::character varying, 'affiliate_activated'::character varying])),
    CONSTRAINT ada_campaign_triggers_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_ada_triggers_tenant ON public.ada_campaign_triggers USING btree (tenant_id);
CREATE INDEX IF NOT EXISTS idx_ada_triggers_trigger ON public.ada_campaign_triggers USING btree (tenant_id, trigger_on, active);

CREATE TABLE IF NOT EXISTS public.affiliates (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    user_id uuid NOT NULL,
    code character varying(50) NOT NULL,
    commission_rate numeric(5,2) DEFAULT 10.00 NOT NULL,
    commission_type character varying(20) DEFAULT 'percentage'::character varying NOT NULL,
    total_earned numeric(12,2) DEFAULT 0 NOT NULL,
    total_paid numeric(12,2) DEFAULT 0 NOT NULL,
    referral_count integer DEFAULT 0 NOT NULL,
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT affiliates_pkey PRIMARY KEY (id),
    CONSTRAINT affiliates_code_key UNIQUE (code),
    CONSTRAINT affiliates_commission_type_check CHECK (commission_type = ANY (ARRAY['percentage'::character varying, 'fixed'::character varying])),
    CONSTRAINT affiliates_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE,
    CONSTRAINT affiliates_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_affiliates_code ON public.affiliates USING btree (code);
CREATE INDEX IF NOT EXISTS idx_affiliates_tenant ON public.affiliates USING btree (tenant_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_affiliates_user_tenant ON public.affiliates USING btree (tenant_id, user_id);

CREATE TABLE IF NOT EXISTS public.app_connections (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    app_slug character varying(100) NOT NULL,
    credentials jsonb DEFAULT '{}'::jsonb NOT NULL,
    config jsonb DEFAULT '{}'::jsonb NOT NULL,
    status character varying(20) DEFAULT 'disconnected'::character varying NOT NULL,
    last_test_at timestamp with time zone,
    last_test_ok boolean,
    error_message text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT app_connections_pkey PRIMARY KEY (id),
    CONSTRAINT app_connections_tenant_id_app_slug_key UNIQUE (tenant_id, app_slug),
    CONSTRAINT app_connections_status_check CHECK (status = ANY (ARRAY['connected'::character varying, 'disconnected'::character varying, 'error'::character varying])),
    CONSTRAINT app_connections_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_app_connections_slug ON public.app_connections USING btree (tenant_id, app_slug);
CREATE INDEX IF NOT EXISTS idx_app_connections_status ON public.app_connections USING btree (tenant_id, status);
CREATE INDEX IF NOT EXISTS idx_app_connections_tenant ON public.app_connections USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.app_sync_logs (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    app_slug character varying(100) NOT NULL,
    direction character varying(10) NOT NULL,
    entity_type character varying(50) NOT NULL,
    records_processed integer DEFAULT 0,
    records_succeeded integer DEFAULT 0,
    records_failed integer DEFAULT 0,
    error_log jsonb,
    started_at timestamp with time zone DEFAULT now() NOT NULL,
    completed_at timestamp with time zone,
    status character varying(20) DEFAULT 'running'::character varying NOT NULL,
    CONSTRAINT app_sync_logs_pkey PRIMARY KEY (id),
    CONSTRAINT app_sync_logs_direction_check CHECK (direction = ANY (ARRAY['push'::character varying, 'pull'::character varying])),
    CONSTRAINT app_sync_logs_status_check CHECK (status = ANY (ARRAY['running'::character varying, 'completed'::character varying, 'failed'::character varying])),
    CONSTRAINT app_sync_logs_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_app_sync_logs_slug ON public.app_sync_logs USING btree (tenant_id, app_slug, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_app_sync_logs_tenant ON public.app_sync_logs USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.audit_logs (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    user_id uuid,
    action character varying(100) NOT NULL,
    entity_type character varying(50) NOT NULL,
    entity_id uuid,
    changes jsonb DEFAULT '{}'::jsonb,
    ip_address character varying(45),
    user_agent text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT audit_logs_pkey PRIMARY KEY (id),
    CONSTRAINT audit_logs_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE,
    CONSTRAINT audit_logs_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_audit_logs_action ON public.audit_logs USING btree (tenant_id, action);
CREATE INDEX IF NOT EXISTS idx_audit_logs_entity ON public.audit_logs USING btree (entity_type, entity_id);
CREATE INDEX IF NOT EXISTS idx_audit_logs_tenant ON public.audit_logs USING btree (tenant_id);
CREATE INDEX IF NOT EXISTS idx_audit_logs_time ON public.audit_logs USING btree (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_audit_logs_user ON public.audit_logs USING btree (user_id);

CREATE TABLE IF NOT EXISTS public.automation_rules (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    name character varying(255) NOT NULL,
    description text,
    trigger_type character varying(50) NOT NULL,
    trigger_config jsonb DEFAULT '{}'::jsonb NOT NULL,
    action_type character varying(50) NOT NULL,
    action_config jsonb DEFAULT '{}'::jsonb NOT NULL,
    is_active boolean DEFAULT true NOT NULL,
    execution_count integer DEFAULT 0,
    last_executed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT automation_rules_pkey PRIMARY KEY (id),
    CONSTRAINT automation_rules_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_automation_rules_active ON public.automation_rules USING btree (tenant_id, is_active);
CREATE INDEX IF NOT EXISTS idx_automation_rules_tenant ON public.automation_rules USING btree (tenant_id);
CREATE INDEX IF NOT EXISTS idx_automation_rules_trigger ON public.automation_rules USING btree (tenant_id, trigger_type);

CREATE TABLE IF NOT EXISTS public.automation_webhooks (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    name character varying(255) NOT NULL,
    webhook_token character varying(255) DEFAULT encode(gen_random_bytes(32), 'hex'::text) NOT NULL,
    allowed_actions text[] DEFAULT '{}'::text[] NOT NULL,
    rate_limit_per_minute integer DEFAULT 60,
    last_used_at timestamp with time zone,
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT automation_webhooks_pkey PRIMARY KEY (id),
    CONSTRAINT automation_webhooks_webhook_token_key UNIQUE (webhook_token),
    CONSTRAINT automation_webhooks_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_automation_webhooks_tenant ON public.automation_webhooks USING btree (tenant_id);
CREATE INDEX IF NOT EXISTS idx_automation_webhooks_token ON public.automation_webhooks USING btree (webhook_token);

CREATE TABLE IF NOT EXISTS public.booking_calendars (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    name text NOT NULL,
    slug text NOT NULL,
    description text,
    calendar_type text DEFAULT 'generic'::text NOT NULL,
    metadata jsonb DEFAULT '{}'::jsonb,
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    google_refresh_token text,
    google_calendar_id text,
    CONSTRAINT booking_calendars_pkey PRIMARY KEY (id),
    CONSTRAINT booking_calendars_tenant_id_slug_key UNIQUE (tenant_id, slug),
    CONSTRAINT booking_calendars_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_booking_calendars_google ON public.booking_calendars USING btree (tenant_id, google_calendar_id) WHERE (google_calendar_id IS NOT NULL);
CREATE INDEX IF NOT EXISTS idx_booking_calendars_slug ON public.booking_calendars USING btree (tenant_id, slug);
CREATE INDEX IF NOT EXISTS idx_booking_calendars_tenant ON public.booking_calendars USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.business_profiles (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid,
    name text,
    slug text,
    category text,
    description text,
    address text,
    phone text,
    website text,
    logo_url text,
    is_claimed boolean DEFAULT false,
    created_at timestamp with time zone DEFAULT now(),
    updated_at timestamp with time zone DEFAULT now(),
    user_id uuid,
    business_name character varying(255),
    unit business_unit,
    current_state user_state DEFAULT 'lead_captured'::user_state,
    subscription_active boolean DEFAULT false,
    last_activity_at timestamp with time zone DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT business_profiles_pkey PRIMARY KEY (id),
    CONSTRAINT business_profiles_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_bp_unit_state_activity ON public.business_profiles USING btree (unit, current_state, last_activity_at) WHERE ((subscription_active = false) AND (current_state = ANY (ARRAY['pending_onboarding'::user_state, 'active'::user_state])));
CREATE INDEX IF NOT EXISTS idx_business_profiles_activity ON public.business_profiles USING btree (last_activity_at) WHERE ((subscription_active = false) AND (current_state = ANY (ARRAY['pending_onboarding'::user_state, 'active'::user_state])));
CREATE INDEX IF NOT EXISTS idx_business_profiles_tenant ON public.business_profiles USING btree (tenant_id);
CREATE INDEX IF NOT EXISTS idx_business_profiles_user ON public.business_profiles USING btree (user_id);

CREATE TABLE IF NOT EXISTS public.calendar_slots (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    calendar_id uuid NOT NULL,
    slot_name text NOT NULL,
    total_slots integer DEFAULT 10 NOT NULL,
    filled_slots integer DEFAULT 0 NOT NULL,
    default_duration_days integer DEFAULT 30 NOT NULL,
    price_override numeric(10,2),
    coreswift_tag_template text,
    coreswift_list_id uuid,
    is_active boolean DEFAULT true NOT NULL,
    sort_order integer DEFAULT 0 NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT calendar_slots_pkey PRIMARY KEY (id),
    CONSTRAINT calendar_slots_calendar_id_fkey FOREIGN KEY (calendar_id) REFERENCES booking_calendars(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_calendar_slots_calendar ON public.calendar_slots USING btree (calendar_id);

CREATE TABLE IF NOT EXISTS public.call_logs (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    caller_number character varying(20) NOT NULL,
    called_number character varying(20) NOT NULL,
    duration integer,
    disposition character varying(20) DEFAULT 'missed'::character varying NOT NULL,
    cost numeric(10,2),
    recorded boolean DEFAULT false,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT call_logs_pkey PRIMARY KEY (id),
    CONSTRAINT call_logs_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_call_logs_tenant ON public.call_logs USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.checklist_templates (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    name character varying(255) NOT NULL,
    description text,
    trigger_type character varying(50) NOT NULL,
    stage_count integer DEFAULT 4 NOT NULL,
    days_per_stage integer DEFAULT 2 NOT NULL,
    is_active boolean DEFAULT true,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT checklist_templates_pkey PRIMARY KEY (id),
    CONSTRAINT checklist_templates_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS public.commission_payouts (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    affiliate_id uuid NOT NULL,
    amount numeric(12,2) NOT NULL,
    status character varying(20) DEFAULT 'pending'::character varying NOT NULL,
    payment_method character varying(50),
    paid_at timestamp with time zone,
    notes text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT commission_payouts_pkey PRIMARY KEY (id),
    CONSTRAINT commission_payouts_status_check CHECK (status = ANY (ARRAY['pending'::character varying, 'processing'::character varying, 'paid'::character varying, 'failed'::character varying])),
    CONSTRAINT commission_payouts_affiliate_id_fkey FOREIGN KEY (affiliate_id) REFERENCES affiliates(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_commission_payouts_affiliate ON public.commission_payouts USING btree (affiliate_id);

CREATE TABLE IF NOT EXISTS public.companies (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    name character varying(255) NOT NULL,
    website character varying(255),
    phone character varying(50),
    email character varying(255),
    industry character varying(100),
    size character varying(50),
    city character varying(100),
    state character varying(100),
    country character varying(100),
    description text,
    metadata jsonb DEFAULT '{}'::jsonb,
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    account_id uuid,
    is_archived boolean DEFAULT false,
    domain character varying(255),
    address_line1 character varying(255),
    address_line2 character varying(255),
    postal_code character varying(50),
    notes text,
    CONSTRAINT companies_pkey PRIMARY KEY (id),
    CONSTRAINT companies_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_companies_domain ON public.companies USING btree (tenant_id, domain);
CREATE INDEX IF NOT EXISTS idx_companies_name ON public.companies USING btree (tenant_id, name);
CREATE INDEX IF NOT EXISTS idx_companies_tenant_id ON public.companies USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.contact_custom_fields (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    key text NOT NULL,
    label text NOT NULL,
    field_type text DEFAULT 'text'::text NOT NULL,
    source_app text DEFAULT 'external'::text NOT NULL,
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT contact_custom_fields_pkey PRIMARY KEY (id),
    CONSTRAINT contact_custom_fields_tenant_id_key_key UNIQUE (tenant_id, key),
    CONSTRAINT contact_custom_fields_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_contact_custom_fields_tenant ON public.contact_custom_fields USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.contacts (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    first_name character varying(100) NOT NULL,
    last_name character varying(100) NOT NULL,
    email character varying(255),
    phone character varying(50),
    company character varying(255),
    job_title character varying(255),
    city character varying(100),
    state character varying(100),
    country character varying(100),
    source character varying(100),
    metadata jsonb DEFAULT '{}'::jsonb,
    score integer DEFAULT 0,
    score_category character varying(20) DEFAULT 'cold'::character varying,
    is_active boolean DEFAULT true,
    last_contacted_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    company_id uuid,
    notes text,
    gender character varying(50),
    address_line1 character varying(255),
    address_line2 character varying(255),
    postal_code character varying(20),
    title character varying(255),
    CONSTRAINT contacts_pkey PRIMARY KEY (id),
    CONSTRAINT contacts_company_id_fkey FOREIGN KEY (company_id) REFERENCES companies(id) ON DELETE SET NULL,
    CONSTRAINT contacts_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_contacts_created_at ON public.contacts USING btree (tenant_id, created_at);
CREATE INDEX IF NOT EXISTS idx_contacts_email ON public.contacts USING btree (email);
CREATE INDEX IF NOT EXISTS idx_contacts_name ON public.contacts USING btree (tenant_id, last_name, first_name);
CREATE INDEX IF NOT EXISTS idx_contacts_score ON public.contacts USING btree (tenant_id, score);
CREATE INDEX IF NOT EXISTS idx_contacts_source ON public.contacts USING btree (tenant_id, source);
CREATE INDEX IF NOT EXISTS idx_contacts_source_app ON public.contacts USING btree (((metadata ->> 'source_app'::text)));
CREATE UNIQUE INDEX IF NOT EXISTS idx_contacts_tenant_email ON public.contacts USING btree (tenant_id, email) WHERE (email IS NOT NULL);
CREATE INDEX IF NOT EXISTS idx_contacts_tenant_id ON public.contacts USING btree (tenant_id);
CREATE INDEX IF NOT EXISTS idx_contacts_tenant_source_created ON public.contacts USING btree (tenant_id, source, created_at DESC);

CREATE TABLE IF NOT EXISTS public.credit_transactions (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    action_type character varying(100) NOT NULL,
    credits integer NOT NULL,
    description text,
    entity_type character varying(50),
    entity_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT credit_transactions_pkey PRIMARY KEY (id),
    CONSTRAINT credit_transactions_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS public.credit_usage (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    period_start timestamp with time zone NOT NULL,
    period_end timestamp with time zone NOT NULL,
    credits_used integer DEFAULT 0 NOT NULL,
    credits_remaining integer DEFAULT 0 NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT credit_usage_pkey PRIMARY KEY (id),
    CONSTRAINT credit_usage_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS public.email_campaigns (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    name text NOT NULL,
    description text,
    status text DEFAULT 'draft'::text NOT NULL,
    created_by uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT email_campaigns_pkey PRIMARY KEY (id),
    CONSTRAINT email_campaigns_status_check CHECK (status = ANY (ARRAY['draft'::text, 'active'::text, 'paused'::text, 'completed'::text, 'archived'::text])),
    CONSTRAINT email_campaigns_created_by_fkey FOREIGN KEY (created_by) REFERENCES users(id),
    CONSTRAINT email_campaigns_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_email_campaigns_status ON public.email_campaigns USING btree (tenant_id, status);
CREATE INDEX IF NOT EXISTS idx_email_campaigns_tenant ON public.email_campaigns USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.email_templates (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    template_type text NOT NULL,
    name text NOT NULL,
    subject text NOT NULL,
    body text,
    html_body text,
    is_default boolean DEFAULT false,
    aid uuid,
    created_at timestamp with time zone DEFAULT now(),
    updated_at timestamp with time zone DEFAULT now(),
    tenant_id uuid,
    body_text text,
    CONSTRAINT email_templates_pkey PRIMARY KEY (id),
    CONSTRAINT email_templates_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_email_templates_unique ON public.email_templates USING btree (template_type, COALESCE(aid, '00000000-0000-0000-0000-000000000000'::uuid), is_default) WHERE ((aid IS NULL) AND (is_default = true));

CREATE TABLE IF NOT EXISTS public.event_logs (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    business_profile_id uuid,
    event_name character varying(100) NOT NULL,
    metadata jsonb,
    created_at timestamp with time zone DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT event_logs_pkey PRIMARY KEY (id),
    CONSTRAINT event_logs_business_profile_id_fkey FOREIGN KEY (business_profile_id) REFERENCES business_profiles(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_event_logs_event ON public.event_logs USING btree (event_name);
CREATE INDEX IF NOT EXISTS idx_event_logs_profile_created ON public.event_logs USING btree (business_profile_id, created_at DESC);

CREATE TABLE IF NOT EXISTS public.followup_queue (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    business_profile_id uuid NOT NULL,
    scheduled_for timestamp with time zone NOT NULL,
    channel channel_type NOT NULL,
    template_slug character varying(100) NOT NULL,
    is_executed boolean DEFAULT false,
    is_cancelled boolean DEFAULT false,
    executed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT followup_queue_pkey PRIMARY KEY (id),
    CONSTRAINT followup_queue_business_profile_id_fkey FOREIGN KEY (business_profile_id) REFERENCES business_profiles(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_followup_queue_profile ON public.followup_queue USING btree (business_profile_id);
CREATE INDEX IF NOT EXISTS idx_fq_scheduled_unexecuted ON public.followup_queue USING btree (scheduled_for) WHERE ((is_executed = false) AND (is_cancelled = false));

CREATE TABLE IF NOT EXISTS public.google_calendar_push_channels (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    booking_calendar_id uuid NOT NULL,
    channel_id text NOT NULL,
    resource_id text NOT NULL,
    token_hash text NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    is_active boolean DEFAULT true NOT NULL,
    last_notification_at timestamp with time zone,
    last_resource_state text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT google_calendar_push_channels_pkey PRIMARY KEY (id),
    CONSTRAINT google_calendar_push_channels_channel_id_key UNIQUE (channel_id),
    CONSTRAINT google_calendar_push_channels_booking_calendar_id_fkey FOREIGN KEY (booking_calendar_id) REFERENCES booking_calendars(id) ON DELETE CASCADE,
    CONSTRAINT google_calendar_push_channels_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_gcal_push_channels_calendar ON public.google_calendar_push_channels USING btree (booking_calendar_id);
CREATE INDEX IF NOT EXISTS idx_gcal_push_channels_tenant ON public.google_calendar_push_channels USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.health_thresholds (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    name character varying(255) NOT NULL,
    entity_type character varying(50) NOT NULL,
    metric character varying(100) NOT NULL,
    operator character varying(5) NOT NULL,
    value integer NOT NULL,
    risk_level character varying(20) DEFAULT 'at_risk'::character varying NOT NULL,
    intervention_action character varying(50) DEFAULT 'send_notification'::character varying NOT NULL,
    intervention_config jsonb DEFAULT '{}'::jsonb,
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT health_thresholds_pkey PRIMARY KEY (id),
    CONSTRAINT health_thresholds_operator_check CHECK (operator = ANY (ARRAY['lt'::character varying, 'gt'::character varying, 'eq'::character varying, 'lte'::character varying, 'gte'::character varying])),
    CONSTRAINT health_thresholds_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS public.inbound_calls (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    caller_number character varying(20) NOT NULL,
    caller_name character varying(255),
    called_number character varying(20) NOT NULL,
    call_time timestamp with time zone DEFAULT now() NOT NULL,
    disposition character varying(20) DEFAULT 'missed'::character varying NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT inbound_calls_pkey PRIMARY KEY (id),
    CONSTRAINT inbound_calls_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_inbound_calls_tenant ON public.inbound_calls USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.integrations (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    name character varying(255) NOT NULL,
    provider character varying(100) NOT NULL,
    config jsonb DEFAULT '{}'::jsonb NOT NULL,
    is_active boolean DEFAULT true NOT NULL,
    last_sync_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT integrations_pkey PRIMARY KEY (id),
    CONSTRAINT integrations_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_integrations_provider ON public.integrations USING btree (tenant_id, provider);
CREATE INDEX IF NOT EXISTS idx_integrations_tenant ON public.integrations USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.lists (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    name character varying(255) NOT NULL,
    description text,
    list_type character varying(20) DEFAULT 'static'::character varying NOT NULL,
    dynamic_rules jsonb DEFAULT '[]'::jsonb,
    member_count integer DEFAULT 0,
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT lists_pkey PRIMARY KEY (id),
    CONSTRAINT lists_list_type_check CHECK (list_type = ANY (ARRAY['static'::character varying, 'dynamic'::character varying])),
    CONSTRAINT lists_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_lists_name_tenant ON public.lists USING btree (tenant_id, name);
CREATE INDEX IF NOT EXISTS idx_lists_tenant_id ON public.lists USING btree (tenant_id);
CREATE INDEX IF NOT EXISTS idx_lists_type ON public.lists USING btree (tenant_id, list_type);

CREATE TABLE IF NOT EXISTS public.message_templates (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    name character varying(255) NOT NULL,
    channel character varying(50) DEFAULT 'email'::character varying,
    subject character varying(500),
    body text,
    variables jsonb DEFAULT '[]'::jsonb,
    created_at timestamp with time zone DEFAULT now(),
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT message_templates_pkey PRIMARY KEY (id),
    CONSTRAINT message_templates_channel_check CHECK (channel = ANY (ARRAY['email'::character varying, 'sms'::character varying, 'whatsapp'::character varying])),
    CONSTRAINT message_templates_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_message_templates_tenant ON public.message_templates USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.module_features (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    module_id uuid NOT NULL,
    key character varying(64) NOT NULL,
    name character varying(120) NOT NULL,
    description text,
    kind text DEFAULT 'boolean'::text NOT NULL,
    unit character varying(32),
    sort_order integer DEFAULT 0 NOT NULL,
    is_active boolean DEFAULT true NOT NULL,
    legacy_feature_key character varying(64),
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT module_features_pkey PRIMARY KEY (id),
    CONSTRAINT module_features_key_key UNIQUE (key),
    CONSTRAINT module_features_kind_check CHECK (kind = ANY (ARRAY['boolean'::text, 'limit'::text])),
    CONSTRAINT module_features_module_id_fkey FOREIGN KEY (module_id) REFERENCES modules(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_module_features_key ON public.module_features USING btree (key);

CREATE TABLE IF NOT EXISTS public.notification_queue (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid,
    user_id uuid,
    type text,
    title text,
    body text,
    data jsonb,
    read boolean DEFAULT false,
    created_at timestamp with time zone DEFAULT now(),
    channel text,
    to_address text,
    status text DEFAULT 'queued'::text,
    subject text,
    template_id uuid,
    template_data jsonb,
    message_id text,
    provider text,
    error_message text,
    retry_count integer DEFAULT 0,
    max_retries integer DEFAULT 3,
    sent_at timestamp with time zone,
    CONSTRAINT notification_queue_pkey PRIMARY KEY (id),
    CONSTRAINT notification_queue_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_notification_queue_status ON public.notification_queue USING btree (status) WHERE (status = 'queued'::text);
CREATE INDEX IF NOT EXISTS idx_notification_queue_tenant ON public.notification_queue USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.notification_rules (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    event_type character varying(100) NOT NULL,
    channel character varying(50) DEFAULT 'in_app'::character varying,
    template_id uuid,
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now(),
    trigger_event text,
    action text,
    target_entity text,
    config jsonb DEFAULT '{}'::jsonb,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT notification_rules_pkey PRIMARY KEY (id),
    CONSTRAINT notification_rules_template_id_fkey FOREIGN KEY (template_id) REFERENCES message_templates(id),
    CONSTRAINT notification_rules_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_notification_rules_tenant ON public.notification_rules USING btree (tenant_id);
CREATE INDEX IF NOT EXISTS idx_notification_rules_trigger ON public.notification_rules USING btree (tenant_id, trigger_event) WHERE (is_active = true);

CREATE TABLE IF NOT EXISTS public.notifications (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    user_id uuid,
    title character varying(255) NOT NULL,
    body text,
    notification_type character varying(50) DEFAULT 'info'::character varying,
    is_read boolean DEFAULT false,
    metadata jsonb DEFAULT '{}'::jsonb,
    created_at timestamp with time zone DEFAULT now(),
    message text,
    read boolean DEFAULT false NOT NULL,
    CONSTRAINT notifications_pkey PRIMARY KEY (id),
    CONSTRAINT notifications_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE,
    CONSTRAINT notifications_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id)
);
CREATE INDEX IF NOT EXISTS idx_notifications_tenant ON public.notifications USING btree (tenant_id);
CREATE INDEX IF NOT EXISTS idx_notifications_user ON public.notifications USING btree (tenant_id, user_id);

CREATE TABLE IF NOT EXISTS public.outbound_messages (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid,
    contact_id uuid,
    channel text,
    recipient text,
    subject text,
    body text,
    status text DEFAULT 'pending'::text,
    scheduled_at timestamp with time zone,
    sent_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now(),
    to_address text,
    cancelled boolean DEFAULT false,
    cc_address text,
    bcc_address text,
    message_id text,
    reply_to text,
    template_id uuid,
    template_data jsonb,
    opened_at timestamp with time zone,
    clicked_at timestamp with time zone,
    error_message text,
    retry_count integer DEFAULT 0,
    max_retries integer DEFAULT 3,
    provider text,
    CONSTRAINT outbound_messages_pkey PRIMARY KEY (id),
    CONSTRAINT outbound_messages_channel_check CHECK (channel = ANY (ARRAY['email'::text, 'sms'::text, 'whatsapp'::text])),
    CONSTRAINT outbound_messages_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_outbound_messages_channel ON public.outbound_messages USING btree (tenant_id, channel);
CREATE INDEX IF NOT EXISTS idx_outbound_messages_created ON public.outbound_messages USING btree (tenant_id, created_at);
CREATE INDEX IF NOT EXISTS idx_outbound_messages_status ON public.outbound_messages USING btree (tenant_id, status);
CREATE INDEX IF NOT EXISTS idx_outbound_messages_tenant ON public.outbound_messages USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.password_resets (
    id uuid NOT NULL,
    user_id uuid NOT NULL,
    token text NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    used boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT password_resets_pkey PRIMARY KEY (id),
    CONSTRAINT password_resets_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_password_resets_token ON public.password_resets USING btree (token);
CREATE INDEX IF NOT EXISTS idx_password_resets_user_id ON public.password_resets USING btree (user_id);

CREATE TABLE IF NOT EXISTS public.personal_api_keys (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    user_id uuid,
    name text DEFAULT 'default'::text NOT NULL,
    key_hash text NOT NULL,
    key_prefix text DEFAULT ''::text NOT NULL,
    is_active boolean DEFAULT true NOT NULL,
    last_used_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT personal_api_keys_pkey PRIMARY KEY (id),
    CONSTRAINT personal_api_keys_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE,
    CONSTRAINT personal_api_keys_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_personal_api_keys_hash ON public.personal_api_keys USING btree (key_hash);
CREATE INDEX IF NOT EXISTS idx_personal_api_keys_tenant ON public.personal_api_keys USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.pipelines (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    name character varying(255) NOT NULL,
    description text,
    is_default boolean DEFAULT false NOT NULL,
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT pipelines_pkey PRIMARY KEY (id),
    CONSTRAINT pipelines_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_pipelines_tenant_id ON public.pipelines USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.plan_module_features (
    plan_id uuid NOT NULL,
    module_feature_id uuid NOT NULL,
    enabled boolean DEFAULT false NOT NULL,
    limit_value numeric,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT plan_module_features_pkey PRIMARY KEY (plan_id, module_feature_id),
    CONSTRAINT plan_module_features_module_feature_id_fkey FOREIGN KEY (module_feature_id) REFERENCES module_features(id) ON DELETE CASCADE,
    CONSTRAINT plan_module_features_plan_id_fkey FOREIGN KEY (plan_id) REFERENCES plans(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_plan_module_features_plan ON public.plan_module_features USING btree (plan_id);

CREATE TABLE IF NOT EXISTS public.plan_modules (
    plan_id uuid NOT NULL,
    module_id uuid NOT NULL,
    enabled boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT plan_modules_pkey PRIMARY KEY (plan_id, module_id),
    CONSTRAINT plan_modules_module_id_fkey FOREIGN KEY (module_id) REFERENCES modules(id) ON DELETE CASCADE,
    CONSTRAINT plan_modules_plan_id_fkey FOREIGN KEY (plan_id) REFERENCES plans(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_plan_modules_plan ON public.plan_modules USING btree (plan_id);

CREATE TABLE IF NOT EXISTS public.portfolio_companies (
    id uuid NOT NULL,
    tenant_id uuid NOT NULL,
    name text NOT NULL,
    slug text NOT NULL,
    email text,
    description text,
    settings jsonb DEFAULT '{}'::jsonb,
    is_active boolean DEFAULT true,
    created_at timestamp with time zone DEFAULT now(),
    updated_at timestamp with time zone DEFAULT now(),
    CONSTRAINT portfolio_companies_pkey PRIMARY KEY (id),
    CONSTRAINT portfolio_companies_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_portfolio_companies_tenant ON public.portfolio_companies USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.prepopulated_data (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    source_url text,
    entity_type character varying(50) NOT NULL,
    entity_id uuid,
    data jsonb DEFAULT '{}'::jsonb NOT NULL,
    preview_link text,
    verified boolean DEFAULT false,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT prepopulated_data_pkey PRIMARY KEY (id),
    CONSTRAINT prepopulated_data_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS public.private_email_api_keys (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    label character varying(128) NOT NULL,
    provider character varying(32) DEFAULT 'mailgun'::character varying NOT NULL,
    api_key_encrypted text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT private_email_api_keys_pkey PRIMARY KEY (id),
    CONSTRAINT private_email_api_keys_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_private_email_api_keys_tenant ON public.private_email_api_keys USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.private_email_domains (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    domain character varying(255) NOT NULL,
    mailgun_api_key text NOT NULL,
    mailgun_region character varying(4) DEFAULT 'us'::character varying NOT NULL,
    catch_all_enabled boolean DEFAULT false NOT NULL,
    verified boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    label character varying(128),
    api_key_id uuid,
    provider_type character varying(32) DEFAULT 'mailgun'::character varying NOT NULL,
    smtp_host character varying,
    smtp_port integer DEFAULT 587,
    smtp_username character varying,
    smtp_password_encrypted text,
    smtp_tls boolean DEFAULT true NOT NULL,
    inbound_mode character varying(16) DEFAULT 'webhook'::character varying NOT NULL,
    webhook_signing_key_encrypted text,
    CONSTRAINT private_email_domains_pkey PRIMARY KEY (id),
    CONSTRAINT private_email_domains_tenant_id_domain_key UNIQUE (tenant_id, domain),
    CONSTRAINT private_email_domains_api_key_id_fkey FOREIGN KEY (api_key_id) REFERENCES private_email_api_keys(id) ON DELETE SET NULL,
    CONSTRAINT private_email_domains_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_private_email_domains_tenant ON public.private_email_domains USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.provider_api_keys (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    label character varying(128) NOT NULL,
    provider character varying(32) NOT NULL,
    access_key_encrypted text,
    secret_key_encrypted text,
    region character varying(16),
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT provider_api_keys_pkey PRIMARY KEY (id),
    CONSTRAINT provider_api_keys_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_provider_api_keys_tenant ON public.provider_api_keys USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.provider_keys (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    provider character varying(64) NOT NULL,
    api_key text NOT NULL,
    base_url character varying(512),
    metadata jsonb DEFAULT '{}'::jsonb,
    is_active boolean DEFAULT true,
    scope character varying(16) DEFAULT 'tenant'::character varying NOT NULL,
    created_at timestamp with time zone DEFAULT now(),
    updated_at timestamp with time zone DEFAULT now(),
    CONSTRAINT provider_keys_pkey PRIMARY KEY (id),
    CONSTRAINT provider_keys_tenant_id_provider_key UNIQUE (tenant_id, provider),
    CONSTRAINT provider_keys_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS public.referrals (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    affiliate_id uuid NOT NULL,
    referred_tenant_id uuid,
    referred_email character varying(255),
    status character varying(20) DEFAULT 'pending'::character varying NOT NULL,
    commission_amount numeric(12,2) DEFAULT 0,
    paid_at timestamp with time zone,
    notes text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT referrals_pkey PRIMARY KEY (id),
    CONSTRAINT referrals_status_check CHECK (status = ANY (ARRAY['pending'::character varying, 'converted'::character varying, 'commissioned'::character varying, 'paid'::character varying, 'expired'::character varying])),
    CONSTRAINT referrals_affiliate_id_fkey FOREIGN KEY (affiliate_id) REFERENCES affiliates(id) ON DELETE CASCADE,
    CONSTRAINT referrals_referred_tenant_id_fkey FOREIGN KEY (referred_tenant_id) REFERENCES tenants(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_referrals_affiliate ON public.referrals USING btree (affiliate_id);
CREATE INDEX IF NOT EXISTS idx_referrals_status ON public.referrals USING btree (status);

CREATE TABLE IF NOT EXISTS public.round_robin_teams (
    id uuid NOT NULL,
    tenant_id uuid NOT NULL,
    name character varying(255) NOT NULL,
    description text,
    strategy character varying(50) DEFAULT 'round_robin'::character varying NOT NULL,
    scope_type character varying(50) DEFAULT 'global'::character varying NOT NULL,
    scope_id uuid,
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT round_robin_teams_pkey PRIMARY KEY (id),
    CONSTRAINT round_robin_teams_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_rr_teams_tenant ON public.round_robin_teams USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.satellite_api_keys (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    user_id uuid,
    name text DEFAULT 'default'::text NOT NULL,
    source_app text DEFAULT 'unknown'::text NOT NULL,
    key_hash text NOT NULL,
    key_prefix text DEFAULT ''::text NOT NULL,
    is_active boolean DEFAULT true NOT NULL,
    last_used_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    permissions jsonb DEFAULT '["read", "write"]'::jsonb,
    CONSTRAINT satellite_api_keys_pkey PRIMARY KEY (id),
    CONSTRAINT satellite_api_keys_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE,
    CONSTRAINT satellite_api_keys_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_satellite_api_keys_prefix ON public.satellite_api_keys USING btree (key_prefix);
CREATE INDEX IF NOT EXISTS idx_satellite_api_keys_tenant ON public.satellite_api_keys USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.score_rules (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    name character varying(255) NOT NULL,
    event_type character varying(100) NOT NULL,
    description text,
    points integer DEFAULT 0 NOT NULL,
    direction character varying(10) DEFAULT 'add'::character varying NOT NULL,
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT score_rules_pkey PRIMARY KEY (id),
    CONSTRAINT score_rules_direction_check CHECK (direction = ANY (ARRAY['add'::character varying, 'subtract'::character varying])),
    CONSTRAINT score_rules_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_score_rules_event ON public.score_rules USING btree (tenant_id, event_type);
CREATE INDEX IF NOT EXISTS idx_score_rules_tenant ON public.score_rules USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.scoring_webhooks (
    id uuid NOT NULL,
    tenant_id uuid NOT NULL,
    name character varying(255) NOT NULL,
    url text NOT NULL,
    min_score integer DEFAULT 0 NOT NULL,
    max_score integer,
    event_type character varying(100),
    headers jsonb DEFAULT '{}'::jsonb,
    is_active boolean DEFAULT true NOT NULL,
    last_fired_at timestamp with time zone,
    failure_count integer DEFAULT 0 NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT scoring_webhooks_pkey PRIMARY KEY (id),
    CONSTRAINT scoring_webhooks_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_scoring_webhooks_score ON public.scoring_webhooks USING btree (tenant_id, min_score);
CREATE INDEX IF NOT EXISTS idx_scoring_webhooks_tenant ON public.scoring_webhooks USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.slot_bookings (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    calendar_id uuid NOT NULL,
    slot_id uuid NOT NULL,
    contact_id uuid,
    business_name text NOT NULL,
    contact_name text,
    contact_email text NOT NULL,
    contact_phone text,
    website text,
    description text,
    target_audience text,
    call_booking text,
    start_date date NOT NULL,
    end_date date NOT NULL,
    slot_position integer NOT NULL,
    status text DEFAULT 'pending_payment'::text NOT NULL,
    price_paid numeric(10,2),
    currency text DEFAULT 'USD'::text NOT NULL,
    stripe_payment_intent_id text,
    stripe_subscription_id text,
    metadata jsonb DEFAULT '{}'::jsonb,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT slot_bookings_pkey PRIMARY KEY (id),
    CONSTRAINT slot_bookings_calendar_id_fkey FOREIGN KEY (calendar_id) REFERENCES booking_calendars(id) ON DELETE CASCADE,
    CONSTRAINT slot_bookings_slot_id_fkey FOREIGN KEY (slot_id) REFERENCES calendar_slots(id) ON DELETE CASCADE,
    CONSTRAINT slot_bookings_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_slot_bookings_active ON public.slot_bookings USING btree (calendar_id, slot_id, status) WHERE (status = 'active'::text);
CREATE INDEX IF NOT EXISTS idx_slot_bookings_dates ON public.slot_bookings USING btree (start_date, end_date);
CREATE INDEX IF NOT EXISTS idx_slot_bookings_status ON public.slot_bookings USING btree (tenant_id, status);
CREATE INDEX IF NOT EXISTS idx_slot_bookings_tenant ON public.slot_bookings USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.support_widgets (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    inbox_id uuid NOT NULL,
    name text NOT NULL,
    slug text NOT NULL,
    theme_color text DEFAULT '#2563eb'::text NOT NULL,
    greeting text DEFAULT 'How can we help?'::text NOT NULL,
    welcome_msg text DEFAULT 'Thanks for reaching out! We will get back to you shortly.'::text NOT NULL,
    position text DEFAULT 'bottom-right'::text NOT NULL,
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT support_widgets_pkey PRIMARY KEY (id),
    CONSTRAINT support_widgets_tenant_id_slug_key UNIQUE (tenant_id, slug),
    CONSTRAINT support_widgets_position_check CHECK ("position" = ANY (ARRAY['bottom-right'::text, 'bottom-left'::text])),
    CONSTRAINT support_widgets_inbox_id_fkey FOREIGN KEY (inbox_id) REFERENCES ticket_inboxes(id) ON DELETE CASCADE,
    CONSTRAINT support_widgets_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_support_widgets_inbox ON public.support_widgets USING btree (inbox_id);
CREATE INDEX IF NOT EXISTS idx_support_widgets_tenant ON public.support_widgets USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.tag_categories (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    name character varying(100) NOT NULL,
    color character varying(7) DEFAULT '#6B7280'::character varying,
    description text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT tag_categories_pkey PRIMARY KEY (id),
    CONSTRAINT tag_categories_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_tag_categories_name_tenant ON public.tag_categories USING btree (tenant_id, name);
CREATE INDEX IF NOT EXISTS idx_tag_categories_tenant ON public.tag_categories USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.tag_sync_log (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    source text NOT NULL,
    target text NOT NULL,
    tag_name text NOT NULL,
    action text NOT NULL,
    status text DEFAULT 'pending'::text NOT NULL,
    error_message text,
    synced_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT tag_sync_log_pkey PRIMARY KEY (id),
    CONSTRAINT tag_sync_log_action_check CHECK (action = ANY (ARRAY['create'::text, 'update'::text, 'delete'::text])),
    CONSTRAINT tag_sync_log_status_check CHECK (status = ANY (ARRAY['pending'::text, 'synced'::text, 'failed'::text])),
    CONSTRAINT tag_sync_log_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS public.tags (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    category_id uuid,
    name character varying(100) NOT NULL,
    color character varying(7) DEFAULT '#6B7280'::character varying,
    parent_id uuid,
    is_dynamic boolean DEFAULT false,
    description text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    is_active boolean DEFAULT true NOT NULL,
    CONSTRAINT tags_pkey PRIMARY KEY (id),
    CONSTRAINT tags_category_id_fkey FOREIGN KEY (category_id) REFERENCES tag_categories(id) ON DELETE SET NULL,
    CONSTRAINT tags_parent_id_fkey FOREIGN KEY (parent_id) REFERENCES tags(id) ON DELETE SET NULL,
    CONSTRAINT tags_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_tags_category ON public.tags USING btree (category_id);
CREATE INDEX IF NOT EXISTS idx_tags_parent ON public.tags USING btree (parent_id);
CREATE INDEX IF NOT EXISTS idx_tags_tenant_id ON public.tags USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.telnyx_numbers (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    phone_number character varying(20) NOT NULL,
    friendly_name character varying(255),
    provider character varying(32) DEFAULT 'telnyx'::character varying NOT NULL,
    capabilities jsonb DEFAULT '{"mms": true, "sms": true, "voice": true}'::jsonb,
    is_active boolean DEFAULT true NOT NULL,
    telnyx_connection_id character varying(255),
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT telnyx_numbers_pkey PRIMARY KEY (id),
    CONSTRAINT telnyx_numbers_phone_number_is_active_key UNIQUE (phone_number, is_active),
    CONSTRAINT telnyx_numbers_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_telnyx_numbers_phone ON public.telnyx_numbers USING btree (phone_number);
CREATE INDEX IF NOT EXISTS idx_telnyx_numbers_tenant ON public.telnyx_numbers USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.tenant_email_limits (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    max_domains integer,
    max_mailboxes integer,
    max_aliases_per_mailbox integer,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    retention_days integer DEFAULT 365 NOT NULL,
    last_purged_at timestamp with time zone,
    CONSTRAINT tenant_email_limits_pkey PRIMARY KEY (id),
    CONSTRAINT tenant_email_limits_tenant_id_key UNIQUE (tenant_id),
    CONSTRAINT tenant_email_limits_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_tenant_email_limits_tenant ON public.tenant_email_limits USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.tenant_invites (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    token character varying(255) NOT NULL,
    role character varying(20) DEFAULT 'member'::character varying NOT NULL,
    accepted boolean DEFAULT false,
    accepted_at timestamp with time zone,
    expires_at timestamp with time zone DEFAULT (now() + '7 days'::interval) NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT tenant_invites_pkey PRIMARY KEY (id),
    CONSTRAINT tenant_invites_token_key UNIQUE (token),
    CONSTRAINT tenant_invites_role_check CHECK (role = ANY (ARRAY['admin'::character varying, 'member'::character varying])),
    CONSTRAINT tenant_invites_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_tenant_invites_tenant ON public.tenant_invites USING btree (tenant_id);
CREATE INDEX IF NOT EXISTS idx_tenant_invites_token ON public.tenant_invites USING btree (token);

CREATE TABLE IF NOT EXISTS public.tenant_plans (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    plan_id uuid NOT NULL,
    status character varying(20) DEFAULT 'active'::character varying NOT NULL,
    billing_cycle character varying(10) DEFAULT 'monthly'::character varying NOT NULL,
    trial_ends_at timestamp with time zone,
    current_period_starts_at timestamp with time zone DEFAULT now() NOT NULL,
    current_period_ends_at timestamp with time zone DEFAULT now() NOT NULL,
    feature_overrides jsonb DEFAULT '{}'::jsonb,
    canceled_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    credit_balance integer DEFAULT 0,
    lifetime_credits integer DEFAULT 0,
    CONSTRAINT tenant_plans_pkey PRIMARY KEY (id),
    CONSTRAINT tenant_plans_tenant_id_key UNIQUE (tenant_id),
    CONSTRAINT tenant_plans_billing_cycle_check CHECK (billing_cycle = ANY (ARRAY['monthly'::character varying, 'yearly'::character varying])),
    CONSTRAINT tenant_plans_status_check CHECK (status = ANY (ARRAY['active'::character varying, 'trialing'::character varying, 'past_due'::character varying, 'canceled'::character varying, 'expired'::character varying])),
    CONSTRAINT tenant_plans_plan_id_fkey FOREIGN KEY (plan_id) REFERENCES plans(id) ON DELETE RESTRICT,
    CONSTRAINT tenant_plans_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_tenant_plans_plan ON public.tenant_plans USING btree (plan_id);
CREATE INDEX IF NOT EXISTS idx_tenant_plans_tenant ON public.tenant_plans USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.tickets (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    subject text NOT NULL,
    description text DEFAULT ''::text NOT NULL,
    status text DEFAULT 'open'::text NOT NULL,
    priority text DEFAULT 'medium'::text NOT NULL,
    assigned_to uuid,
    contact_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    source text DEFAULT 'manual'::text NOT NULL,
    contact_email text,
    contact_name text,
    widget_id uuid,
    CONSTRAINT tickets_pkey PRIMARY KEY (id),
    CONSTRAINT tickets_priority_check CHECK (priority = ANY (ARRAY['low'::text, 'medium'::text, 'high'::text, 'urgent'::text])),
    CONSTRAINT tickets_source_check CHECK (source = ANY (ARRAY['manual'::text, 'email'::text, 'form'::text, 'agent'::text, 'portal'::text])),
    CONSTRAINT tickets_status_check CHECK (status = ANY (ARRAY['open'::text, 'in_progress'::text, 'resolved'::text, 'closed'::text])),
    CONSTRAINT tickets_assigned_to_fkey FOREIGN KEY (assigned_to) REFERENCES contacts(id) ON DELETE SET NULL,
    CONSTRAINT tickets_contact_id_fkey FOREIGN KEY (contact_id) REFERENCES contacts(id) ON DELETE CASCADE,
    CONSTRAINT tickets_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE,
    CONSTRAINT tickets_widget_id_fkey FOREIGN KEY (widget_id) REFERENCES support_widgets(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_tickets_contact_id ON public.tickets USING btree (contact_id);
CREATE INDEX IF NOT EXISTS idx_tickets_status ON public.tickets USING btree (status);
CREATE INDEX IF NOT EXISTS idx_tickets_tenant_id ON public.tickets USING btree (tenant_id);
CREATE INDEX IF NOT EXISTS idx_tickets_widget_id ON public.tickets USING btree (widget_id);

CREATE TABLE IF NOT EXISTS public.tracked_links (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    tag_id uuid NOT NULL,
    slug text NOT NULL,
    target_url text NOT NULL,
    created_at timestamp with time zone DEFAULT now(),
    CONSTRAINT tracked_links_pkey PRIMARY KEY (id),
    CONSTRAINT tracked_links_slug_key UNIQUE (slug),
    CONSTRAINT tracked_links_tag_id_fkey FOREIGN KEY (tag_id) REFERENCES tags(id),
    CONSTRAINT tracked_links_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_tracked_links_slug ON public.tracked_links USING btree (slug);
CREATE INDEX IF NOT EXISTS idx_tracked_links_tenant ON public.tracked_links USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.user_industry_dashboards (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    tenant_id uuid NOT NULL,
    industry_slug character varying(100) NOT NULL,
    dashboard_name character varying(255) NOT NULL,
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT user_industry_dashboards_pkey PRIMARY KEY (id),
    CONSTRAINT user_industry_dashboards_user_id_industry_slug_key UNIQUE (user_id, industry_slug),
    CONSTRAINT user_industry_dashboards_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE,
    CONSTRAINT user_industry_dashboards_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_user_industry_dashboards_industry ON public.user_industry_dashboards USING btree (industry_slug);
CREATE INDEX IF NOT EXISTS idx_user_industry_dashboards_tenant ON public.user_industry_dashboards USING btree (tenant_id);
CREATE INDEX IF NOT EXISTS idx_user_industry_dashboards_user ON public.user_industry_dashboards USING btree (user_id);

CREATE TABLE IF NOT EXISTS public.affiliate_products (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    name character varying(255) NOT NULL,
    description text,
    price numeric(10,2) DEFAULT 0 NOT NULL,
    commission_rate numeric(5,2) DEFAULT 10.00,
    commission_type character varying(20) DEFAULT 'percentage'::character varying NOT NULL,
    commission_amount numeric(10,2) DEFAULT 0,
    tag_id uuid,
    image_url text,
    checkout_url text,
    is_active boolean DEFAULT true NOT NULL,
    sort_order integer DEFAULT 0,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT affiliate_products_pkey PRIMARY KEY (id),
    CONSTRAINT affiliate_products_commission_type_check CHECK (commission_type = ANY (ARRAY['percentage'::character varying, 'fixed'::character varying])),
    CONSTRAINT affiliate_products_tag_id_fkey FOREIGN KEY (tag_id) REFERENCES tags(id) ON DELETE SET NULL,
    CONSTRAINT affiliate_products_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_affiliate_products_active ON public.affiliate_products USING btree (tenant_id, is_active);
CREATE INDEX IF NOT EXISTS idx_affiliate_products_tag ON public.affiliate_products USING btree (tag_id);
CREATE INDEX IF NOT EXISTS idx_affiliate_products_tenant ON public.affiliate_products USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.automation_webhook_logs (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    webhook_id uuid,
    action character varying(100) NOT NULL,
    request_body jsonb,
    response_status integer,
    response_body text,
    ip_address inet,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT automation_webhook_logs_pkey PRIMARY KEY (id),
    CONSTRAINT automation_webhook_logs_webhook_id_fkey FOREIGN KEY (webhook_id) REFERENCES automation_webhooks(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_webhook_logs_created ON public.automation_webhook_logs USING btree (webhook_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_webhook_logs_webhook ON public.automation_webhook_logs USING btree (webhook_id);

CREATE TABLE IF NOT EXISTS public.checklist_instances (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    template_id uuid NOT NULL,
    entity_type character varying(50) NOT NULL,
    entity_id uuid NOT NULL,
    current_stage integer DEFAULT 0,
    completed boolean DEFAULT false,
    started_at timestamp with time zone DEFAULT now() NOT NULL,
    completed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT checklist_instances_pkey PRIMARY KEY (id),
    CONSTRAINT checklist_instances_template_id_fkey FOREIGN KEY (template_id) REFERENCES checklist_templates(id),
    CONSTRAINT checklist_instances_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS public.checklist_progress (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    instance_id uuid NOT NULL,
    stage_order integer NOT NULL,
    completed boolean DEFAULT false,
    action_taken character varying(100),
    completed_at timestamp with time zone,
    sent_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT checklist_progress_pkey PRIMARY KEY (id),
    CONSTRAINT checklist_progress_instance_id_fkey FOREIGN KEY (instance_id) REFERENCES checklist_instances(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS public.checklist_stages (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    template_id uuid NOT NULL,
    stage_order integer NOT NULL,
    title character varying(255) NOT NULL,
    description text,
    action_required character varying(100),
    channel character varying(10) DEFAULT 'email'::character varying,
    message_template text,
    delay_hours integer DEFAULT 0,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT checklist_stages_pkey PRIMARY KEY (id),
    CONSTRAINT checklist_stages_channel_check CHECK (channel = ANY (ARRAY['email'::character varying, 'sms'::character varying, 'both'::character varying])),
    CONSTRAINT checklist_stages_template_id_fkey FOREIGN KEY (template_id) REFERENCES checklist_templates(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS public.contact_field_values (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    contact_id uuid NOT NULL,
    field_id uuid NOT NULL,
    value text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT contact_field_values_pkey PRIMARY KEY (id),
    CONSTRAINT contact_field_values_contact_id_field_id_key UNIQUE (contact_id, field_id),
    CONSTRAINT contact_field_values_contact_id_fkey FOREIGN KEY (contact_id) REFERENCES contacts(id) ON DELETE CASCADE,
    CONSTRAINT contact_field_values_field_id_fkey FOREIGN KEY (field_id) REFERENCES contact_custom_fields(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_contact_field_values_contact ON public.contact_field_values USING btree (contact_id);
CREATE INDEX IF NOT EXISTS idx_contact_field_values_field ON public.contact_field_values USING btree (field_id);

CREATE TABLE IF NOT EXISTS public.email_campaign_enrollments (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    campaign_id uuid NOT NULL,
    entity_type text DEFAULT 'contact'::text NOT NULL,
    entity_id uuid NOT NULL,
    current_step integer DEFAULT 0 NOT NULL,
    total_steps integer NOT NULL,
    status text DEFAULT 'active'::text NOT NULL,
    next_send_at timestamp with time zone,
    started_at timestamp with time zone DEFAULT now() NOT NULL,
    completed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT email_campaign_enrollments_pkey PRIMARY KEY (id),
    CONSTRAINT email_campaign_enrollments_status_check CHECK (status = ANY (ARRAY['active'::text, 'paused'::text, 'completed'::text, 'unsubscribed'::text])),
    CONSTRAINT email_campaign_enrollments_campaign_id_fkey FOREIGN KEY (campaign_id) REFERENCES email_campaigns(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_email_campaign_enrollments_campaign ON public.email_campaign_enrollments USING btree (campaign_id);
CREATE INDEX IF NOT EXISTS idx_email_campaign_enrollments_entity ON public.email_campaign_enrollments USING btree (entity_type, entity_id);
CREATE INDEX IF NOT EXISTS idx_email_campaign_enrollments_next_send ON public.email_campaign_enrollments USING btree (next_send_at) WHERE ((status = 'active'::text) AND (next_send_at IS NOT NULL));

CREATE TABLE IF NOT EXISTS public.email_campaign_steps (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    campaign_id uuid NOT NULL,
    step_order integer NOT NULL,
    template_name text NOT NULL,
    subject text,
    body text NOT NULL,
    delay_days integer DEFAULT 0 NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT email_campaign_steps_pkey PRIMARY KEY (id),
    CONSTRAINT email_campaign_steps_campaign_id_fkey FOREIGN KEY (campaign_id) REFERENCES email_campaigns(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_email_campaign_steps_campaign ON public.email_campaign_steps USING btree (campaign_id);

CREATE TABLE IF NOT EXISTS public.email_campaign_triggers (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    campaign_id uuid NOT NULL,
    tag_id uuid NOT NULL,
    trigger_type text DEFAULT 'tag_assigned'::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT email_campaign_triggers_pkey PRIMARY KEY (id),
    CONSTRAINT email_campaign_triggers_campaign_id_tag_id_key UNIQUE (campaign_id, tag_id),
    CONSTRAINT email_campaign_triggers_trigger_type_check CHECK (trigger_type = ANY (ARRAY['tag_assigned'::text, 'contact_created'::text, 'manual'::text])),
    CONSTRAINT email_campaign_triggers_campaign_id_fkey FOREIGN KEY (campaign_id) REFERENCES email_campaigns(id) ON DELETE CASCADE,
    CONSTRAINT email_campaign_triggers_tag_id_fkey FOREIGN KEY (tag_id) REFERENCES tags(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS public.inbound_webhook_events (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    source_app text NOT NULL,
    event_type text NOT NULL,
    event_payload jsonb DEFAULT '{}'::jsonb NOT NULL,
    api_key_id uuid,
    status text DEFAULT 'received'::text NOT NULL,
    error_message text,
    processed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT inbound_webhook_events_pkey PRIMARY KEY (id),
    CONSTRAINT inbound_webhook_events_status_check CHECK (status = ANY (ARRAY['received'::text, 'processed'::text, 'failed'::text, 'ignored'::text])),
    CONSTRAINT inbound_webhook_events_api_key_id_fkey FOREIGN KEY (api_key_id) REFERENCES satellite_api_keys(id) ON DELETE SET NULL,
    CONSTRAINT inbound_webhook_events_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_inbound_webhook_events_created ON public.inbound_webhook_events USING btree (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_inbound_webhook_events_source ON public.inbound_webhook_events USING btree (tenant_id, source_app);
CREATE INDEX IF NOT EXISTS idx_inbound_webhook_events_status ON public.inbound_webhook_events USING btree (tenant_id, status);
CREATE INDEX IF NOT EXISTS idx_inbound_webhook_events_tenant ON public.inbound_webhook_events USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.integration_targets (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    portfolio_company_id uuid,
    user_id uuid,
    name text NOT NULL,
    provider text DEFAULT 'webhook'::text NOT NULL,
    webhook_url text NOT NULL,
    api_key text,
    events text[] DEFAULT '{}'::text[],
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT integration_targets_pkey PRIMARY KEY (id),
    CONSTRAINT integration_targets_portfolio_company_id_fkey FOREIGN KEY (portfolio_company_id) REFERENCES portfolio_companies(id) ON DELETE CASCADE,
    CONSTRAINT integration_targets_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE,
    CONSTRAINT integration_targets_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_integration_targets_tenant ON public.integration_targets USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.link_clicks (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tracked_link_id uuid NOT NULL,
    contact_id uuid NOT NULL,
    tenant_id uuid NOT NULL,
    clicked_at timestamp with time zone DEFAULT now(),
    CONSTRAINT link_clicks_pkey PRIMARY KEY (id),
    CONSTRAINT link_clicks_contact_id_fkey FOREIGN KEY (contact_id) REFERENCES contacts(id),
    CONSTRAINT link_clicks_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE,
    CONSTRAINT link_clicks_tracked_link_id_fkey FOREIGN KEY (tracked_link_id) REFERENCES tracked_links(id)
);
CREATE INDEX IF NOT EXISTS idx_link_clicks_clicked ON public.link_clicks USING btree (clicked_at);
CREATE INDEX IF NOT EXISTS idx_link_clicks_contact ON public.link_clicks USING btree (contact_id);
CREATE INDEX IF NOT EXISTS idx_link_clicks_link ON public.link_clicks USING btree (tracked_link_id);

CREATE TABLE IF NOT EXISTS public.list_members (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    list_id uuid NOT NULL,
    contact_id uuid NOT NULL,
    added_by uuid,
    added_at timestamp with time zone DEFAULT now() NOT NULL,
    tenant_id uuid,
    added_manually boolean DEFAULT false NOT NULL,
    CONSTRAINT list_members_pkey PRIMARY KEY (id),
    CONSTRAINT list_members_list_id_contact_id_key UNIQUE (list_id, contact_id),
    CONSTRAINT list_members_added_by_fkey FOREIGN KEY (added_by) REFERENCES users(id) ON DELETE SET NULL,
    CONSTRAINT list_members_contact_id_fkey FOREIGN KEY (contact_id) REFERENCES contacts(id) ON DELETE CASCADE,
    CONSTRAINT list_members_list_id_fkey FOREIGN KEY (list_id) REFERENCES lists(id) ON DELETE CASCADE,
    CONSTRAINT list_members_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_list_members_contact ON public.list_members USING btree (contact_id);
CREATE INDEX IF NOT EXISTS idx_list_members_list ON public.list_members USING btree (list_id);

CREATE TABLE IF NOT EXISTS public.pipeline_stages (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    pipeline_id uuid NOT NULL,
    name character varying(255) NOT NULL,
    color character varying(7) DEFAULT '#6B7280'::character varying,
    sort_order integer DEFAULT 0 NOT NULL,
    probability integer DEFAULT 0,
    is_won boolean DEFAULT false NOT NULL,
    is_lost boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT pipeline_stages_pkey PRIMARY KEY (id),
    CONSTRAINT pipeline_stages_pipeline_id_fkey FOREIGN KEY (pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_pipeline_stages_pipeline ON public.pipeline_stages USING btree (pipeline_id);

CREATE TABLE IF NOT EXISTS public.private_email_boxes (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    domain_id uuid NOT NULL,
    user_id uuid,
    local_part character varying(64) NOT NULL,
    email_address character varying(255) NOT NULL,
    mailgun_mailbox_id character varying(255),
    forwarding_enabled boolean DEFAULT true NOT NULL,
    signature text,
    status character varying(32) DEFAULT 'active'::character varying NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT private_email_boxes_pkey PRIMARY KEY (id),
    CONSTRAINT private_email_boxes_tenant_id_email_address_key UNIQUE (tenant_id, email_address),
    CONSTRAINT private_email_boxes_domain_id_fkey FOREIGN KEY (domain_id) REFERENCES private_email_domains(id) ON DELETE CASCADE,
    CONSTRAINT private_email_boxes_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE,
    CONSTRAINT private_email_boxes_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_private_email_boxes_domain ON public.private_email_boxes USING btree (domain_id);
CREATE INDEX IF NOT EXISTS idx_private_email_boxes_tenant ON public.private_email_boxes USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.round_robin_members (
    id uuid NOT NULL,
    team_id uuid NOT NULL,
    user_id uuid NOT NULL,
    weight integer DEFAULT 1 NOT NULL,
    max_concurrent_bookings integer DEFAULT 10,
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT round_robin_members_pkey PRIMARY KEY (id),
    CONSTRAINT round_robin_members_team_id_user_id_key UNIQUE (team_id, user_id),
    CONSTRAINT round_robin_members_team_id_fkey FOREIGN KEY (team_id) REFERENCES round_robin_teams(id) ON DELETE CASCADE,
    CONSTRAINT round_robin_members_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_rr_members_team ON public.round_robin_members USING btree (team_id);

CREATE TABLE IF NOT EXISTS public.score_history (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    contact_id uuid NOT NULL,
    rule_id uuid,
    event_type character varying(100),
    points integer NOT NULL,
    previous_score integer DEFAULT 0 NOT NULL,
    new_score integer DEFAULT 0 NOT NULL,
    reason text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT score_history_pkey PRIMARY KEY (id),
    CONSTRAINT score_history_contact_id_fkey FOREIGN KEY (contact_id) REFERENCES contacts(id) ON DELETE CASCADE,
    CONSTRAINT score_history_rule_id_fkey FOREIGN KEY (rule_id) REFERENCES score_rules(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_score_history_contact ON public.score_history USING btree (contact_id);
CREATE INDEX IF NOT EXISTS idx_score_history_time ON public.score_history USING btree (created_at);

CREATE TABLE IF NOT EXISTS public.scoring_thresholds (
    id uuid NOT NULL,
    tenant_id uuid NOT NULL,
    pipeline_id uuid NOT NULL,
    min_score integer NOT NULL,
    max_score integer,
    target_stage_id uuid NOT NULL,
    action text DEFAULT 'move_stage'::text NOT NULL,
    action_config jsonb DEFAULT '{}'::jsonb,
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT scoring_thresholds_pkey PRIMARY KEY (id),
    CONSTRAINT scoring_thresholds_pipeline_id_fkey FOREIGN KEY (pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE,
    CONSTRAINT scoring_thresholds_target_stage_id_fkey FOREIGN KEY (target_stage_id) REFERENCES pipeline_stages(id) ON DELETE CASCADE,
    CONSTRAINT scoring_thresholds_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_scoring_thresholds_score ON public.scoring_thresholds USING btree (tenant_id, min_score);
CREATE INDEX IF NOT EXISTS idx_scoring_thresholds_tenant ON public.scoring_thresholds USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.tag_assignments (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tag_id uuid NOT NULL,
    entity_type character varying(50) NOT NULL,
    entity_id uuid NOT NULL,
    assigned_by uuid,
    assigned_at timestamp with time zone DEFAULT now() NOT NULL,
    tenant_id uuid,
    CONSTRAINT tag_assignments_pkey PRIMARY KEY (id),
    CONSTRAINT tag_assignments_tag_id_entity_type_entity_id_tenant_id_key UNIQUE (tag_id, entity_type, entity_id, tenant_id),
    CONSTRAINT tag_assignments_assigned_by_fkey FOREIGN KEY (assigned_by) REFERENCES users(id) ON DELETE SET NULL,
    CONSTRAINT tag_assignments_tag_id_fkey FOREIGN KEY (tag_id) REFERENCES tags(id) ON DELETE CASCADE,
    CONSTRAINT tag_assignments_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_tag_assignments_entity ON public.tag_assignments USING btree (entity_type, entity_id);
CREATE INDEX IF NOT EXISTS idx_tag_assignments_tag ON public.tag_assignments USING btree (tag_id);

CREATE TABLE IF NOT EXISTS public.tag_mappings (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    integration_id uuid NOT NULL,
    tag_id uuid NOT NULL,
    external_system character varying(100) NOT NULL,
    external_id character varying(255) NOT NULL,
    external_name character varying(255),
    direction character varying(20) DEFAULT 'bidirectional'::character varying NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT tag_mappings_pkey PRIMARY KEY (id),
    CONSTRAINT tag_mappings_direction_check CHECK (direction = ANY (ARRAY['outbound'::character varying, 'inbound'::character varying, 'bidirectional'::character varying])),
    CONSTRAINT tag_mappings_integration_id_fkey FOREIGN KEY (integration_id) REFERENCES integrations(id) ON DELETE CASCADE,
    CONSTRAINT tag_mappings_tag_id_fkey FOREIGN KEY (tag_id) REFERENCES tags(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_tag_mappings_integration ON public.tag_mappings USING btree (integration_id);
CREATE INDEX IF NOT EXISTS idx_tag_mappings_tag ON public.tag_mappings USING btree (tag_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_tag_mappings_unique ON public.tag_mappings USING btree (integration_id, tag_id, external_system);

CREATE TABLE IF NOT EXISTS public.ticket_messages (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    ticket_id uuid NOT NULL,
    sender_type text NOT NULL,
    message text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT ticket_messages_pkey PRIMARY KEY (id),
    CONSTRAINT ticket_messages_sender_type_check CHECK (sender_type = ANY (ARRAY['agent'::text, 'contact'::text])),
    CONSTRAINT ticket_messages_ticket_id_fkey FOREIGN KEY (ticket_id) REFERENCES tickets(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_ticket_messages_ticket_id ON public.ticket_messages USING btree (ticket_id);

CREATE TABLE IF NOT EXISTS public.affiliate_product_selections (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    affiliate_id uuid NOT NULL,
    product_id uuid NOT NULL,
    is_active boolean DEFAULT true,
    promo_link text,
    custom_commission_rate numeric(5,2),
    selected_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT affiliate_product_selections_pkey PRIMARY KEY (id),
    CONSTRAINT affiliate_product_selections_affiliate_id_product_id_key UNIQUE (affiliate_id, product_id),
    CONSTRAINT affiliate_product_selections_affiliate_id_fkey FOREIGN KEY (affiliate_id) REFERENCES affiliates(id) ON DELETE CASCADE,
    CONSTRAINT affiliate_product_selections_product_id_fkey FOREIGN KEY (product_id) REFERENCES affiliate_products(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_aps_active ON public.affiliate_product_selections USING btree (affiliate_id, is_active);
CREATE INDEX IF NOT EXISTS idx_aps_affiliate ON public.affiliate_product_selections USING btree (affiliate_id);
CREATE INDEX IF NOT EXISTS idx_aps_product ON public.affiliate_product_selections USING btree (product_id);

CREATE TABLE IF NOT EXISTS public.opportunities (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    tenant_id uuid NOT NULL,
    pipeline_id uuid NOT NULL,
    stage_id uuid NOT NULL,
    contact_id uuid,
    company_id uuid,
    name character varying(255) NOT NULL,
    value numeric(15,2) DEFAULT 0,
    currency character varying(3) DEFAULT 'USD'::character varying,
    probability integer DEFAULT 0,
    expected_close_date date,
    source character varying(100),
    notes text,
    metadata jsonb DEFAULT '{}'::jsonb,
    is_won boolean DEFAULT false,
    is_lost boolean DEFAULT false,
    lost_reason text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT opportunities_pkey PRIMARY KEY (id),
    CONSTRAINT opportunities_company_id_fkey FOREIGN KEY (company_id) REFERENCES companies(id) ON DELETE SET NULL,
    CONSTRAINT opportunities_contact_id_fkey FOREIGN KEY (contact_id) REFERENCES contacts(id) ON DELETE SET NULL,
    CONSTRAINT opportunities_pipeline_id_fkey FOREIGN KEY (pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE,
    CONSTRAINT opportunities_stage_id_fkey FOREIGN KEY (stage_id) REFERENCES pipeline_stages(id) ON DELETE RESTRICT,
    CONSTRAINT opportunities_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_opportunities_contact ON public.opportunities USING btree (contact_id);
CREATE INDEX IF NOT EXISTS idx_opportunities_pipeline ON public.opportunities USING btree (pipeline_id);
CREATE INDEX IF NOT EXISTS idx_opportunities_stage ON public.opportunities USING btree (stage_id);
CREATE INDEX IF NOT EXISTS idx_opportunities_tenant_id ON public.opportunities USING btree (tenant_id);
CREATE INDEX IF NOT EXISTS idx_opportunities_value ON public.opportunities USING btree (tenant_id, value);

CREATE TABLE IF NOT EXISTS public.opportunity_stage_history (
    id uuid DEFAULT uuid_generate_v4() NOT NULL,
    opportunity_id uuid NOT NULL,
    from_stage_id uuid,
    to_stage_id uuid NOT NULL,
    moved_by uuid,
    moved_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT opportunity_stage_history_pkey PRIMARY KEY (id),
    CONSTRAINT opportunity_stage_history_from_stage_id_fkey FOREIGN KEY (from_stage_id) REFERENCES pipeline_stages(id) ON DELETE SET NULL,
    CONSTRAINT opportunity_stage_history_moved_by_fkey FOREIGN KEY (moved_by) REFERENCES users(id) ON DELETE SET NULL,
    CONSTRAINT opportunity_stage_history_opportunity_id_fkey FOREIGN KEY (opportunity_id) REFERENCES opportunities(id) ON DELETE CASCADE,
    CONSTRAINT opportunity_stage_history_to_stage_id_fkey FOREIGN KEY (to_stage_id) REFERENCES pipeline_stages(id) ON DELETE SET NULL
);
CREATE INDEX IF NOT EXISTS idx_stage_history_opportunity ON public.opportunity_stage_history USING btree (opportunity_id);
CREATE INDEX IF NOT EXISTS idx_stage_history_time ON public.opportunity_stage_history USING btree (moved_at);

CREATE TABLE IF NOT EXISTS public.private_email_auto_replies (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    domain_id uuid NOT NULL,
    mailbox_id uuid,
    name character varying(255) NOT NULL,
    trigger_type character varying(32) NOT NULL,
    trigger_value character varying(255),
    subject character varying(255),
    body_html text NOT NULL,
    delay_minutes integer DEFAULT 0 NOT NULL,
    is_active boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT private_email_auto_replies_pkey PRIMARY KEY (id),
    CONSTRAINT private_email_auto_replies_domain_id_fkey FOREIGN KEY (domain_id) REFERENCES private_email_domains(id) ON DELETE CASCADE,
    CONSTRAINT private_email_auto_replies_mailbox_id_fkey FOREIGN KEY (mailbox_id) REFERENCES private_email_boxes(id) ON DELETE CASCADE,
    CONSTRAINT private_email_auto_replies_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_private_email_auto_replies_tenant ON public.private_email_auto_replies USING btree (tenant_id);

CREATE TABLE IF NOT EXISTS public.round_robin_assignments (
    id uuid NOT NULL,
    team_id uuid NOT NULL,
    member_id uuid NOT NULL,
    booking_id uuid,
    contact_id uuid,
    assigned_at timestamp with time zone DEFAULT now() NOT NULL,
    status character varying(50) DEFAULT 'pending'::character varying NOT NULL,
    CONSTRAINT round_robin_assignments_pkey PRIMARY KEY (id),
    CONSTRAINT round_robin_assignments_booking_id_fkey FOREIGN KEY (booking_id) REFERENCES slot_bookings(id) ON DELETE SET NULL,
    CONSTRAINT round_robin_assignments_member_id_fkey FOREIGN KEY (member_id) REFERENCES round_robin_members(id) ON DELETE CASCADE,
    CONSTRAINT round_robin_assignments_team_id_fkey FOREIGN KEY (team_id) REFERENCES round_robin_teams(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_rr_assignments_member ON public.round_robin_assignments USING btree (member_id);
CREATE INDEX IF NOT EXISTS idx_rr_assignments_team ON public.round_robin_assignments USING btree (team_id);

CREATE TABLE IF NOT EXISTS public.deal_reminders (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    opportunity_id uuid NOT NULL,
    user_id uuid NOT NULL,
    remind_at timestamp with time zone NOT NULL,
    reminder_type text DEFAULT 'manual'::text NOT NULL,
    note text,
    is_dismissed boolean DEFAULT false NOT NULL,
    dismissed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT deal_reminders_pkey PRIMARY KEY (id),
    CONSTRAINT deal_reminders_opportunity_id_fkey FOREIGN KEY (opportunity_id) REFERENCES opportunities(id) ON DELETE CASCADE,
    CONSTRAINT deal_reminders_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE,
    CONSTRAINT deal_reminders_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_deal_reminders_opportunity ON public.deal_reminders USING btree (opportunity_id);
CREATE INDEX IF NOT EXISTS idx_deal_reminders_remind_at ON public.deal_reminders USING btree (remind_at) WHERE (NOT is_dismissed);
CREATE INDEX IF NOT EXISTS idx_deal_reminders_tenant ON public.deal_reminders USING btree (tenant_id);
CREATE INDEX IF NOT EXISTS idx_deal_reminders_user ON public.deal_reminders USING btree (user_id);

CREATE TABLE IF NOT EXISTS public.events (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    title character varying(255) NOT NULL,
    description text,
    event_date date,
    start_time time without time zone,
    end_time time without time zone,
    location character varying(255),
    event_type character varying(50) DEFAULT 'meeting'::character varying,
    status character varying(50) DEFAULT 'scheduled'::character varying,
    contact_id uuid,
    company_id uuid,
    deal_id uuid,
    created_by uuid,
    created_at timestamp with time zone DEFAULT now(),
    updated_at timestamp with time zone DEFAULT now(),
    source character varying(100),
    entity_type character varying(50),
    entity_id uuid,
    payload jsonb DEFAULT '{}'::jsonb NOT NULL,
    raw_headers jsonb,
    processed boolean DEFAULT false NOT NULL,
    processed_at timestamp with time zone,
    CONSTRAINT events_pkey PRIMARY KEY (id),
    CONSTRAINT events_company_id_fkey FOREIGN KEY (company_id) REFERENCES companies(id),
    CONSTRAINT events_contact_id_fkey FOREIGN KEY (contact_id) REFERENCES contacts(id),
    CONSTRAINT events_created_by_fkey FOREIGN KEY (created_by) REFERENCES users(id),
    CONSTRAINT events_deal_id_fkey FOREIGN KEY (deal_id) REFERENCES opportunities(id),
    CONSTRAINT events_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_events_created ON public.events USING btree (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_events_date ON public.events USING btree (event_date);
CREATE INDEX IF NOT EXISTS idx_events_email_created ON public.events USING btree (tenant_id, created_at) WHERE ((source)::text = 'private_email'::text);
CREATE INDEX IF NOT EXISTS idx_events_entity ON public.events USING btree (tenant_id, entity_type, entity_id);
CREATE INDEX IF NOT EXISTS idx_events_source ON public.events USING btree (tenant_id, source);
CREATE INDEX IF NOT EXISTS idx_events_tenant ON public.events USING btree (tenant_id);
CREATE INDEX IF NOT EXISTS idx_events_type ON public.events USING btree (tenant_id, event_type);

CREATE TABLE IF NOT EXISTS public.delayed_actions (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid,
    action_type text,
    payload jsonb,
    execute_at timestamp with time zone,
    executed boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone DEFAULT now(),
    cancelled boolean DEFAULT false NOT NULL,
    status text DEFAULT 'pending'::text,
    error_message text,
    trigger_event_id uuid,
    condition_type character varying(20),
    condition_config jsonb DEFAULT '{}'::jsonb NOT NULL,
    action_config jsonb DEFAULT '{}'::jsonb NOT NULL,
    result jsonb,
    entity_type character varying(50),
    entity_id uuid,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT delayed_actions_pkey PRIMARY KEY (id),
    CONSTRAINT delayed_actions_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_delayed_actions_entity ON public.delayed_actions USING btree (tenant_id, entity_type, entity_id);
CREATE INDEX IF NOT EXISTS idx_delayed_actions_execute ON public.delayed_actions USING btree (tenant_id, execute_at) WHERE ((executed = false) AND (cancelled = false));
CREATE INDEX IF NOT EXISTS idx_delayed_actions_tenant ON public.delayed_actions USING btree (tenant_id);
