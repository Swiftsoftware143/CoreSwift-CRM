-- CoreSwift data-driven MODULE & FEATURE registry (CS-25).
--
-- Every tool the CRM ships (calendar, private mailbox, SMS/Telnyx, scoring, campaigns, …) becomes a
-- `modules` row; every capability of that tool becomes a `module_features` row; and the ADMIN assigns
-- both to plans through `plan_modules` / `plan_module_features`. Nothing about entitlement lives in
-- Rust any more — `src/features.rs` used to hold a `FEATURE_REGISTRY` const plus a `PLAN_KEY_ALIASES`
-- table whose own comment admitted that drift made the gate "silently stop enforcing".
--
-- THE 6 EXISTING PLAN ROWS ARE NOT TOUCHED. `plans`, `plans.features`, names, prices and limits keep
-- exactly the values they have today. This migration only ADDS tables and derives their seed from the
-- live `plans`/`plans.features` data, once, so that the resolved entitlement of every plan x module is
-- identical before and after the switch.
--
-- Idempotent: every statement is IF NOT EXISTS / ON CONFLICT DO NOTHING, so a re-run cannot clobber an
-- assignment the admin has since made by hand.

CREATE TABLE IF NOT EXISTS modules (
    id                 uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    key                varchar(64) NOT NULL UNIQUE,
    name               varchar(120) NOT NULL,
    description        text,
    icon               varchar(32),
    sort_order         integer NOT NULL DEFAULT 0,
    is_active          boolean NOT NULL DEFAULT true,
    -- The spelling this module's flag uses in the legacy `plans.features` JSONB. Kept as DATA (not a
    -- Rust const) because the plans predate the registry and four of them spell the key differently.
    legacy_feature_key varchar(64),
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS module_features (
    id                 uuid PRIMARY KEY DEFAULT uuid_generate_v4(),
    module_id          uuid NOT NULL REFERENCES modules(id) ON DELETE CASCADE,
    key                varchar(64) NOT NULL UNIQUE,
    name               varchar(120) NOT NULL,
    description        text,
    kind               text NOT NULL DEFAULT 'boolean' CHECK (kind IN ('boolean', 'limit')),
    unit               varchar(32),
    sort_order         integer NOT NULL DEFAULT 0,
    is_active          boolean NOT NULL DEFAULT true,
    legacy_feature_key varchar(64),
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS plan_modules (
    plan_id    uuid NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
    module_id  uuid NOT NULL REFERENCES modules(id) ON DELETE CASCADE,
    enabled    boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (plan_id, module_id)
);

CREATE TABLE IF NOT EXISTS plan_module_features (
    plan_id           uuid NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
    module_feature_id uuid NOT NULL REFERENCES module_features(id) ON DELETE CASCADE,
    enabled           boolean NOT NULL DEFAULT false,
    limit_value       numeric,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (plan_id, module_feature_id)
);

CREATE INDEX IF NOT EXISTS idx_modules_key ON modules (key);
CREATE INDEX IF NOT EXISTS idx_module_features_key ON module_features (key);
CREATE INDEX IF NOT EXISTS idx_plan_modules_plan ON plan_modules (plan_id);
CREATE INDEX IF NOT EXISTS idx_plan_module_features_plan ON plan_module_features (plan_id);

-- ---------------------------------------------------------------------------
-- 1. the module catalogue: the 21 gate_mw-wired / registry modules + a limits module
-- ---------------------------------------------------------------------------
INSERT INTO modules (key, name, description, icon, sort_order, legacy_feature_key) VALUES
    ('campaigns',       'Campaigns',              'Sequenced email campaigns',                       '🎯',  10, NULL),
    ('automation',      'Automations',            'Trigger/action engine',                           '⚙️',  20, 'automation_enabled'),
    ('checklists',      'Checklists',             'Onboarding and process checklists',               '✅',  30, 'onboarding_checklists'),
    ('ai_enabled',      'AI scoring & helpers',   'Lead scoring, prioritisation and AI helpers',     '🤖',  40, NULL),
    ('tickets',         'Support tickets',        'In-house ticketing and email-to-ticket',          '🎫',  50, 'support_tickets'),
    ('affiliates',      'Affiliate system',       'Referral tracking and payouts',                   '🤝',  60, NULL),
    ('native_apps',     'Native app connectors',  'FunnelSwift, ADASwift, MissedCall, WorkflowSwift, CheatLayer', '🔌', 70, NULL),
    ('telnyx',          'SMS & voice (Telnyx)',   'SMS, number management, call tracking',           '📱',  80, NULL),
    ('round_robin',     'Round-robin routing',    'Fair lead distribution across a team',            '🔄',  90, NULL),
    ('events',          'Event system',           'Internal event bus',                              '📡', 100, NULL),
    ('monitoring',      'Monitoring & health',    'Account health scoring and thresholds',           '💓', 110, 'account_health_monitoring'),
    ('bookings',        'Bookings & scheduling',  'Calendar booking pages',                          '📅', 120, NULL),
    ('google_calendar', 'Google Calendar sync',   'Two-way calendar sync',                           '🗓️', 130, NULL),
    ('api_access',      'API access',             'Per-tenant API keys',                             '🔑', 140, NULL),
    ('webhooks',        'Webhooks',               'Outbound webhook delivery',                       '🪝', 150, NULL),
    ('integrations',    'Integrations',           'n8n and third-party integrations',                '🧩', 160, NULL),
    ('provider_keys',   'Provider keys',          'Bring-your-own provider credentials',             '🔐', 170, NULL),
    ('support_widgets', 'Support widgets',        'Embeddable support surfaces',                     '💬', 180, NULL),
    ('tracked_links',   'Tracked links',          'Click tracking links',                            '🔗', 190, NULL),
    ('private_email',   'Private mailbox',        'Own domain + mailboxes (also has its own limits)', '📬', 200, NULL),
    ('portfolio',       'Portfolio sync',         'Cross-tenant portfolio management',               '🏢', 210, NULL),
    ('limits',          'Plan limits & quotas',   'Numeric ceilings the plan grants',                '📊', 220, NULL)
ON CONFLICT (key) DO NOTHING;

-- ---------------------------------------------------------------------------
-- 2. per-module features: each module owns an on/off feature named after itself, private mailbox
--    carries its two real limits, and the limits module carries the numeric ceilings.
-- ---------------------------------------------------------------------------
INSERT INTO module_features (module_id, key, name, kind, unit, sort_order, legacy_feature_key)
SELECT m.id, m.key, m.name, 'boolean', NULL, 0, m.legacy_feature_key
  FROM modules m
 WHERE m.key <> 'limits'
ON CONFLICT (key) DO NOTHING;

INSERT INTO module_features (module_id, key, name, description, kind, unit, sort_order, legacy_feature_key)
SELECT m.id, v.fkey, v.fname, v.fdesc, v.kind, v.unit, v.so, v.legacy
  FROM modules m
  JOIN (VALUES
        ('private_email', 'email_domains',   'Custom domains',        'Domains this plan may host',        'limit', 'domains',   10, NULL),
        ('private_email', 'email_mailboxes', 'Mailboxes',             'Mailboxes this plan may create',    'limit', 'mailboxes', 20, NULL),
        ('limits', 'limit_max_users',           'Max users',            'Active team members',               'limit', 'users',   10, 'max_users'),
        ('limits', 'limit_max_contacts',        'Max contacts',         'Contact records',                   'limit', 'contacts', 20, 'max_contacts'),
        ('limits', 'limit_pipelines',           'Pipelines',            'Pipeline boards',                   'limit', 'pipelines', 30, 'pipelines'),
        ('limits', 'limit_integrations',        'Integrations',         'Integration targets',               'limit', 'integrations', 40, 'integrations'),
        ('limits', 'limit_max_widgets',         'Support widgets',      'Embeddable widgets',                'limit', 'widgets',  50, 'max_widgets'),
        ('limits', 'limit_storage_gb',          'Storage',              'Storage allowance',                 'limit', 'GB',       60, 'storage_gb'),
        ('limits', 'limit_api_calls_per_day',   'API calls per day',    'Daily API ceiling',                 'limit', 'calls/day', 70, 'api_calls_per_day'),
        ('limits', 'limit_max_industries',      'Industries',           'Industry tabs',                     'limit', 'industries', 80, NULL),
        ('limits', 'limit_monthly_credits',     'Monthly credits',      'Run credits granted each month',    'limit', 'credits',  90, NULL)
       ) AS v(mkey, fkey, fname, fdesc, kind, unit, so, legacy) ON v.mkey = m.key
ON CONFLICT (key) DO NOTHING;

-- ---------------------------------------------------------------------------
-- 3. SEED plan_modules to mirror TODAY's effective entitlement exactly.
--    Old semantics (enforce_feature_flag): read plans.features under the alias spelling, coerce with
--    as_bool(); a non-boolean or absent value fell through to ALLOWED. So:
--        enabled = the plan's explicit boolean if it has one, otherwise TRUE.
--    DO NOTHING on conflict so a re-run never overwrites an admin's assignment.
-- ---------------------------------------------------------------------------
INSERT INTO plan_modules (plan_id, module_id, enabled)
SELECT p.id,
       m.id,
       COALESCE(
           CASE WHEN jsonb_typeof(p.features -> COALESCE(m.legacy_feature_key, m.key)) = 'boolean'
                THEN (p.features ->> COALESCE(m.legacy_feature_key, m.key))::boolean
           END,
           true)
  FROM plans p
 CROSS JOIN modules m
ON CONFLICT (plan_id, module_id) DO NOTHING;

-- David's explicit requirement (2026-09-22): the private mailbox is NOT part of the free plan.
-- This is the ONE cell where the seed deliberately differs from today's fail-open behaviour; every
-- PAID tier keeps private mailbox granted, so no paying tenant loses anything.
UPDATE plan_modules pm
   SET enabled = false, updated_at = now()
  FROM modules m, plans p
 WHERE pm.module_id = m.id AND pm.plan_id = p.id
   AND m.key = 'private_email' AND p.slug = 'free';

-- ---------------------------------------------------------------------------
-- 4. per-feature rows for each plan: each module's own feature mirrors its module assignment.
-- ---------------------------------------------------------------------------
INSERT INTO plan_module_features (plan_id, module_feature_id, enabled, limit_value)
SELECT pm.plan_id, f.id, pm.enabled, NULL
  FROM plan_modules pm
  JOIN modules m ON m.id = pm.module_id
  JOIN module_features f ON f.module_id = m.id AND f.key = m.key
ON CONFLICT (plan_id, module_feature_id) DO NOTHING;

-- the private mailbox limits exist on every plan but carry no configured value yet (mirrors today's
-- data: `tenant_email_limits` is empty and no plan defines a domain/mailbox key). The admin fills
-- these in; this migration must not invent pricing.
INSERT INTO plan_module_features (plan_id, module_feature_id, enabled, limit_value)
SELECT p.id, f.id, true, NULL
  FROM plans p
  JOIN modules m ON m.key = 'private_email'
  JOIN module_features f ON f.module_id = m.id AND f.kind = 'limit'
ON CONFLICT (plan_id, module_feature_id) DO NOTHING;

-- ---------------------------------------------------------------------------
-- 5. the numeric ceilings, read once from the live plan data (JSONB + dedicated columns).
-- ---------------------------------------------------------------------------
INSERT INTO plan_module_features (plan_id, module_feature_id, enabled, limit_value)
SELECT p.id, f.id, (s.v IS NOT NULL), s.v
  FROM plans p
  JOIN modules m ON m.key = 'limits'
  JOIN module_features f ON f.module_id = m.id
 CROSS JOIN LATERAL (
        SELECT CASE f.key
                   WHEN 'limit_max_users'         THEN (p.features ->> 'max_users')::numeric
                   WHEN 'limit_max_contacts'      THEN (p.features ->> 'max_contacts')::numeric
                   WHEN 'limit_pipelines'         THEN (p.features ->> 'pipelines')::numeric
                   WHEN 'limit_integrations'      THEN (p.features ->> 'integrations')::numeric
                   WHEN 'limit_max_widgets'       THEN (p.features ->> 'max_widgets')::numeric
                   WHEN 'limit_storage_gb'        THEN (p.features ->> 'storage_gb')::numeric
                   WHEN 'limit_api_calls_per_day' THEN (p.features ->> 'api_calls_per_day')::numeric
                   WHEN 'limit_max_industries'    THEN p.max_industries::numeric
                   WHEN 'limit_monthly_credits'   THEN p.monthly_credits::numeric
               END AS v
     ) AS s
ON CONFLICT (plan_id, module_feature_id) DO NOTHING;
