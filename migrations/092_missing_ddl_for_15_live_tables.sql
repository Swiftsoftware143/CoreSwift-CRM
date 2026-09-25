-- 092_missing_ddl_for_15_live_tables.sql
-- CoreSwift-CRM: the 15 tables that exist in the live database but carry NO CREATE TABLE in
-- migrations/*.sql, so a database built from this repository could not reproduce the live schema
-- (card t_d8b2e888).  Found while writing 091, whose api_keys/cs_messages arms had to be guarded
-- with to_regclass() for exactly this reason.
--
-- Source of truth: the LIVE catalog (coreswift_crm, 2026-09-25).  Every statement is idempotent
-- (CREATE TABLE IF NOT EXISTS / CREATE INDEX IF NOT EXISTS), so this file is a NO-OP on the live
-- database and reproduces the live definition exactly on a fresh one.
--
-- Dependency order matters: plan_tiers before business_subscriptions/city_plan_slots,
-- networks before network_branding, api_keys before api_key_usage, cs_messages self-FK inline.
--
-- Measured disposition of all 15 (see /opt/swift/audits/cs-missing-ddl-t_d8b2e888/):
--   LIVE, reader in this repo  : admin_settings (SELECT/INSERT src/admin_actions/site_handler.rs),
--                                cs_messages (full CRUD src/messages/handlers.rs), and their FK shape
--   DEAD structure, 0 rows,    : the other 12 - inherited from the WorkflowSwift/multi-directory
--   0 code references            lineage, all 0 rows, no reader anywhere in any fleet repo.  They are
--                                REPRODUCED rather than DROPped: dropping is destructive, the card's
--                                goal is reproducibility, and networks/plan_tiers/business_subscriptions
--                                carry this app's own integration/billing columns
--                                (networks.coreswift_tenant_id, networks.coreswift_list_id_*),
--                                so their fate is an owner decision, not a schema-parity side effect.
--
-- 091 stays as it is (already applied, checksummed) and its to_regclass() guards simply stop firing
-- on a fresh database once these tables exist; the ON DELETE CASCADE it installs for
-- api_keys.tenant_id / cs_messages.tenant_id is declared inline here so the guarantee holds on a
-- fresh build without 091 having to run against a table it cannot see.
--
-- No BEGIN/COMMIT: sqlx runs each migration file inside its own transaction, and no other file in
-- this repo opens one.

-- ----------------------------------------------------------------------------
-- plan_tiers   (live: 25 columns, 2 constraints, 0 non-constraint indexes)
CREATE TABLE IF NOT EXISTS public.plan_tiers (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    name text NOT NULL,
    slug text NOT NULL,
    price_monthly numeric(10,2) DEFAULT 0,
    price_yearly numeric(10,2) DEFAULT 0,
    max_listings integer DEFAULT '-1'::integer,
    max_deals integer DEFAULT 0,
    max_photos integer DEFAULT 5,
    has_reviews boolean DEFAULT true,
    has_analytics boolean DEFAULT false,
    has_crm boolean DEFAULT false,
    has_email boolean DEFAULT false,
    has_call_tracking boolean DEFAULT false,
    has_import_export boolean DEFAULT false,
    has_api_access boolean DEFAULT false,
    featured_listing boolean DEFAULT false,
    description text,
    created_at timestamp with time zone DEFAULT now(),
    max_industries integer DEFAULT 1,
    coreswift_tag text,
    coreswift_pipeline_stage text,
    slot_duration_days integer DEFAULT 30,
    max_slots_per_city integer,
    plan_sales_page_url text,
    payment_provider character varying(64),
    CONSTRAINT plan_tiers_pkey PRIMARY KEY (id),
    CONSTRAINT plan_tiers_slug_key UNIQUE (slug)
);

-- ----------------------------------------------------------------------------
-- networks   (live: 14 columns, 3 constraints, 0 non-constraint indexes)
CREATE TABLE IF NOT EXISTS public.networks (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    name character varying(255) NOT NULL,
    slug character varying(100) NOT NULL,
    description text,
    root_domain character varying(255),
    status character varying(20) DEFAULT 'active'::character varying,
    owner_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    coreswift_tenant_id uuid,
    coreswift_key_prefix text,
    coreswift_list_id_sponsors uuid,
    coreswift_list_id_claimed uuid,
    coreswift_list_id_newsletter uuid,
    CONSTRAINT networks_pkey PRIMARY KEY (id),
    CONSTRAINT networks_slug_key UNIQUE (slug),
    CONSTRAINT networks_owner_id_fkey FOREIGN KEY (owner_id) REFERENCES users(id) ON DELETE SET NULL
);

-- ----------------------------------------------------------------------------
-- api_keys   (live: 14 columns, 4 constraints, 3 non-constraint indexes)
CREATE TABLE IF NOT EXISTS public.api_keys (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    user_id uuid NOT NULL,
    tenant_id uuid,
    name text NOT NULL,
    key_hash text NOT NULL,
    key_prefix text NOT NULL,
    scopes text[] DEFAULT '{}'::text[],
    rate_limit_per_minute integer DEFAULT 60,
    rate_limit_per_hour integer DEFAULT 1000,
    is_active boolean DEFAULT true,
    last_used_at timestamp with time zone,
    expires_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now(),
    updated_at timestamp with time zone DEFAULT now(),
    CONSTRAINT api_keys_pkey PRIMARY KEY (id),
    CONSTRAINT api_keys_key_hash_key UNIQUE (key_hash),
    CONSTRAINT api_keys_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE,
    CONSTRAINT api_keys_user_id_fkey FOREIGN KEY (user_id) REFERENCES users(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_api_keys_key_hash ON public.api_keys USING btree (key_hash);
CREATE INDEX IF NOT EXISTS idx_api_keys_prefix ON public.api_keys USING btree (key_prefix);
CREATE INDEX IF NOT EXISTS idx_api_keys_user ON public.api_keys USING btree (user_id);

-- ----------------------------------------------------------------------------
-- admin_settings   (live: 4 columns, 1 constraints, 0 non-constraint indexes)
CREATE TABLE IF NOT EXISTS public.admin_settings (
    key text NOT NULL,
    value jsonb DEFAULT '{}'::jsonb NOT NULL,
    description text,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT admin_settings_pkey PRIMARY KEY (key)
);

-- ----------------------------------------------------------------------------
-- legal_pages   (live: 8 columns, 1 constraints, 2 non-constraint indexes)
CREATE TABLE IF NOT EXISTS public.legal_pages (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    title text NOT NULL,
    page_type text DEFAULT 'custom'::text NOT NULL,
    content text NOT NULL,
    published boolean DEFAULT true,
    is_global boolean DEFAULT false,
    created_at timestamp with time zone DEFAULT now(),
    updated_at timestamp with time zone DEFAULT now(),
    CONSTRAINT legal_pages_pkey PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_legal_pages_published ON public.legal_pages USING btree (published);
CREATE INDEX IF NOT EXISTS idx_legal_pages_type ON public.legal_pages USING btree (page_type);

-- ----------------------------------------------------------------------------
-- seo_meta   (live: 13 columns, 2 constraints, 1 non-constraint indexes)
CREATE TABLE IF NOT EXISTS public.seo_meta (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    page_type text NOT NULL,
    page_id uuid,
    title text,
    description text,
    keywords text,
    og_image text,
    og_title text,
    og_description text,
    schema_type text,
    custom_schema jsonb,
    created_at timestamp with time zone DEFAULT now(),
    updated_at timestamp with time zone DEFAULT now(),
    CONSTRAINT seo_meta_pkey PRIMARY KEY (id),
    CONSTRAINT seo_meta_page_type_page_id_key UNIQUE (page_type, page_id)
);
CREATE INDEX IF NOT EXISTS idx_seo_meta_page ON public.seo_meta USING btree (page_type, page_id);

-- ----------------------------------------------------------------------------
-- tag_rules   (live: 10 columns, 4 constraints, 2 non-constraint indexes)
CREATE TABLE IF NOT EXISTS public.tag_rules (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    name text NOT NULL,
    tag_id uuid NOT NULL,
    trigger_type text NOT NULL,
    action_type text NOT NULL,
    action_config jsonb DEFAULT '{}'::jsonb NOT NULL,
    is_active boolean DEFAULT true,
    created_at timestamp with time zone DEFAULT now(),
    updated_at timestamp with time zone DEFAULT now(),
    CONSTRAINT tag_rules_pkey PRIMARY KEY (id),
    CONSTRAINT tag_rules_action_type_check CHECK (action_type = ANY (ARRAY['send_email'::text, 'send_sms'::text, 'webhook'::text, 'pipeline_move'::text, 'scoring_update'::text, 'add_tag'::text, 'remove_tag'::text, 'issue_voucher'::text])),
    CONSTRAINT tag_rules_trigger_type_check CHECK (trigger_type = ANY (ARRAY['tag_applied'::text, 'tag_removed'::text, 'workflow_completed'::text])),
    CONSTRAINT tag_rules_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_tag_rules_tag ON public.tag_rules USING btree (tag_id);
CREATE INDEX IF NOT EXISTS idx_tag_rules_tenant ON public.tag_rules USING btree (tenant_id);

-- ----------------------------------------------------------------------------
-- google_places_cache   (live: 17 columns, 1 constraints, 2 non-constraint indexes)
CREATE TABLE IF NOT EXISTS public.google_places_cache (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    query text NOT NULL,
    place_id text,
    name text,
    formatted_address text,
    phone text,
    website text,
    latitude double precision,
    longitude double precision,
    rating double precision,
    user_ratings_total integer,
    types text[],
    photos text[],
    opening_hours jsonb,
    place_details jsonb,
    cached_at timestamp with time zone DEFAULT now(),
    expires_at timestamp with time zone DEFAULT (now() + '7 days'::interval),
    CONSTRAINT google_places_cache_pkey PRIMARY KEY (id)
);
CREATE INDEX IF NOT EXISTS idx_google_places_cache_place_id ON public.google_places_cache USING btree (place_id);
CREATE INDEX IF NOT EXISTS idx_google_places_cache_query ON public.google_places_cache USING btree (query);

-- ----------------------------------------------------------------------------
-- payment_providers   (live: 12 columns, 3 constraints, 1 non-constraint indexes)
CREATE TABLE IF NOT EXISTS public.payment_providers (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    provider_type text NOT NULL,
    label text DEFAULT ''::text NOT NULL,
    is_active boolean DEFAULT false NOT NULL,
    api_key_encrypted text,
    webhook_secret_encrypted text,
    config jsonb DEFAULT '{}'::jsonb NOT NULL,
    publishable_key text,
    webhook_path text,
    is_test_mode boolean DEFAULT true NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT payment_providers_pkey PRIMARY KEY (id),
    CONSTRAINT payment_providers_provider_type_key UNIQUE (provider_type),
    CONSTRAINT payment_providers_provider_type_check CHECK (provider_type = ANY (ARRAY['stripe'::text, 'paypal'::text, 'square'::text, 'paddle'::text]))
);
CREATE INDEX IF NOT EXISTS idx_payment_providers_active ON public.payment_providers USING btree (is_active) WHERE (is_active = true);

-- ----------------------------------------------------------------------------
-- payment_webhook_events   (live: 9 columns, 2 constraints, 2 non-constraint indexes)
CREATE TABLE IF NOT EXISTS public.payment_webhook_events (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    provider_type text NOT NULL,
    event_type text,
    event_id text,
    raw_body jsonb,
    headers jsonb,
    status text DEFAULT 'received'::text NOT NULL,
    error_message text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT payment_webhook_events_pkey PRIMARY KEY (id),
    CONSTRAINT payment_webhook_events_status_check CHECK (status = ANY (ARRAY['received'::text, 'processed'::text, 'failed'::text, 'ignored'::text]))
);
CREATE INDEX IF NOT EXISTS idx_payment_webhook_events_provider ON public.payment_webhook_events USING btree (provider_type);
CREATE INDEX IF NOT EXISTS idx_payment_webhook_events_status ON public.payment_webhook_events USING btree (status);

-- ----------------------------------------------------------------------------
-- business_subscriptions   (live: 13 columns, 2 constraints, 0 non-constraint indexes)
CREATE TABLE IF NOT EXISTS public.business_subscriptions (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    business_id uuid NOT NULL,
    tier_id uuid,
    status text DEFAULT 'active'::text,
    billing_cycle text DEFAULT 'monthly'::text,
    price_paid numeric(10,2),
    currency text DEFAULT 'USD'::text,
    start_date date NOT NULL,
    end_date date,
    auto_renew boolean DEFAULT true,
    stripe_subscription_id text,
    created_at timestamp with time zone DEFAULT now(),
    external_payment_ref text,
    CONSTRAINT business_subscriptions_pkey PRIMARY KEY (id),
    CONSTRAINT business_subscriptions_tier_id_fkey FOREIGN KEY (tier_id) REFERENCES plan_tiers(id)
);

-- ----------------------------------------------------------------------------
-- city_plan_slots   (live: 5 columns, 3 constraints, 0 non-constraint indexes)
CREATE TABLE IF NOT EXISTS public.city_plan_slots (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    city_slug text NOT NULL,
    plan_tier_id uuid NOT NULL,
    total_slots integer DEFAULT 10 NOT NULL,
    filled_slots integer DEFAULT 0 NOT NULL,
    CONSTRAINT city_plan_slots_pkey PRIMARY KEY (id),
    CONSTRAINT city_plan_slots_city_slug_plan_tier_id_key UNIQUE (city_slug, plan_tier_id),
    CONSTRAINT city_plan_slots_plan_tier_id_fkey FOREIGN KEY (plan_tier_id) REFERENCES plan_tiers(id) ON DELETE CASCADE
);

-- ----------------------------------------------------------------------------
-- network_branding   (live: 15 columns, 3 constraints, 0 non-constraint indexes)
CREATE TABLE IF NOT EXISTS public.network_branding (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    network_id uuid NOT NULL,
    logo_url text,
    logo_footer_url text,
    favicon_url text,
    primary_color character varying(7) DEFAULT '#2563eb'::character varying,
    secondary_color character varying(7) DEFAULT '#64748b'::character varying,
    accent_color character varying(7) DEFAULT '#f59e0b'::character varying,
    background_color character varying(7) DEFAULT '#ffffff'::character varying,
    text_color character varying(7) DEFAULT '#1e293b'::character varying,
    heading_color character varying(7) DEFAULT '#0f172a'::character varying,
    heading_font character varying(100) DEFAULT 'Inter'::character varying,
    body_font character varying(100) DEFAULT 'Inter'::character varying,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT network_branding_pkey PRIMARY KEY (id),
    CONSTRAINT network_branding_network_id_key UNIQUE (network_id),
    CONSTRAINT network_branding_network_id_fkey FOREIGN KEY (network_id) REFERENCES networks(id) ON DELETE CASCADE
);

-- ----------------------------------------------------------------------------
-- api_key_usage   (live: 8 columns, 2 constraints, 2 non-constraint indexes)
CREATE TABLE IF NOT EXISTS public.api_key_usage (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    api_key_id uuid NOT NULL,
    endpoint text NOT NULL,
    method text NOT NULL,
    status_code integer,
    ip_address text,
    response_time_ms integer,
    created_at timestamp with time zone DEFAULT now(),
    CONSTRAINT api_key_usage_pkey PRIMARY KEY (id),
    CONSTRAINT api_key_usage_api_key_id_fkey FOREIGN KEY (api_key_id) REFERENCES api_keys(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_api_key_usage_created ON public.api_key_usage USING btree (created_at);
CREATE INDEX IF NOT EXISTS idx_api_key_usage_key ON public.api_key_usage USING btree (api_key_id);

-- ----------------------------------------------------------------------------
-- cs_messages   (live: 16 columns, 5 constraints, 3 non-constraint indexes)
CREATE TABLE IF NOT EXISTS public.cs_messages (
    id uuid DEFAULT gen_random_uuid() NOT NULL,
    tenant_id uuid NOT NULL,
    sender_id uuid,
    recipient_id uuid,
    subject character varying(255),
    body text,
    channel character varying(50) DEFAULT 'email'::character varying,
    status character varying(50) DEFAULT 'sent'::character varying,
    read boolean DEFAULT false,
    direction character varying(20) DEFAULT 'outbound'::character varying,
    thread_id uuid,
    parent_message_id uuid,
    metadata jsonb DEFAULT '{}'::jsonb,
    created_at timestamp with time zone DEFAULT now(),
    updated_at timestamp with time zone DEFAULT now(),
    is_archived boolean DEFAULT false,
    CONSTRAINT cs_messages_pkey PRIMARY KEY (id),
    CONSTRAINT cs_messages_parent_message_id_fkey FOREIGN KEY (parent_message_id) REFERENCES cs_messages(id),
    CONSTRAINT cs_messages_recipient_id_fkey FOREIGN KEY (recipient_id) REFERENCES contacts(id),
    CONSTRAINT cs_messages_sender_id_fkey FOREIGN KEY (sender_id) REFERENCES users(id),
    CONSTRAINT cs_messages_tenant_id_fkey FOREIGN KEY (tenant_id) REFERENCES tenants(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_cs_messages_recipient ON public.cs_messages USING btree (recipient_id);
CREATE INDEX IF NOT EXISTS idx_cs_messages_status ON public.cs_messages USING btree (status);
CREATE INDEX IF NOT EXISTS idx_cs_messages_tenant ON public.cs_messages USING btree (tenant_id);

-- ------------------------------------------------------------------------------------------------
-- Convergence: the two things a from-zero build reaches differently from live, measured by
-- diffing the whole catalog of a from-zero build against live (see the audit directory).
-- Both are no-ops on live (`SET DEFAULT` to the value live already holds, `ADD VALUE IF NOT EXISTS`,
-- `VALIDATE CONSTRAINT` on a constraint live already has validated).
-- ------------------------------------------------------------------------------------------------

-- 1. 000_baseline_live_schema.sql cannot declare `users.role`'s live default ('user'::user_role)
--    because 002 creates the user_role enum with a bare CREATE TYPE, so the baseline emits the same
--    value cast to text and this restores live's exact default expression now that the enum exists.
ALTER TABLE users ALTER COLUMN role SET DEFAULT 'user'::user_role;

-- 2. Live's user_role enum carries three labels no migration adds (added out of band).  Code casts
--    role text to user_role and compares against 'owner' (src/admin_actions/handlers.rs), so a
--    from-zero install without them would reject the platform-admin path.
ALTER TYPE user_role ADD VALUE IF NOT EXISTS 'company_admin';
ALTER TYPE user_role ADD VALUE IF NOT EXISTS 'owner';
ALTER TYPE user_role ADD VALUE IF NOT EXISTS 'member';

-- 3. Four `sealed` CHECK guards are VALIDATED on live.  The baseline cannot declare them (075/077/
--    078/079 add them without a DROP ... IF EXISTS first), the files add them NOT VALID, so the
--    fresh build has to validate them the way live did.
ALTER TABLE provider_keys VALIDATE CONSTRAINT provider_keys_api_key_sealed;
ALTER TABLE integration_targets VALIDATE CONSTRAINT integration_targets_api_key_sealed;
ALTER TABLE webhook_endpoints VALIDATE CONSTRAINT webhook_endpoints_secret_sealed;
ALTER TABLE booking_calendars VALIDATE CONSTRAINT booking_calendars_google_token_sealed;
