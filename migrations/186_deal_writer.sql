-- 186_deal_writer.sql — the DEAL WRITER: software-wide deal core, per-workspace layer.
--
-- David approved 2026-11-20 (kanban t_2df68850). The deal entity already exists as
-- `opportunities` (pipelines + stages + history). This migration ADDS what the writer and the
-- deal tab need without forking a second system of record:
--
--   * external reference on every API-created deal (unique per workspace) — one external ref,
--     one deal; a re-send updates instead of duplicating;
--   * owner + collaborators;
--   * a configurable lost-reason list per workspace;
--   * per-workspace deal custom fields + values (the INDUSTRY layer: courier fields for the
--     Xride account live here, never in the core);
--   * a deal activity timeline (separate from the calendar's `events` table);
--   * tasks/follow-ups with due dates, and file attachments;
--   * stage automation hooks (entering a stage can tag the deal or call a workflow URL);
--   * timestamps that make "rotting in a stage" measurable.
--
-- Idempotent: every statement is IF NOT EXISTS / ON CONFLICT DO NOTHING, so a re-run is a no-op.

ALTER TABLE opportunities
    ADD COLUMN IF NOT EXISTS external_ref text,
    ADD COLUMN IF NOT EXISTS owner_id uuid REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN IF NOT EXISTS last_activity_at timestamptz NOT NULL DEFAULT now(),
    ADD COLUMN IF NOT EXISTS stage_entered_at timestamptz NOT NULL DEFAULT now(),
    -- MULTI-CURRENCY IS CORE (David, 2026-11-20). The rate is captured AT WRITE TIME, so a deal
    -- won in March keeps March's rate and a past quarter does not move when the market does.
    ADD COLUMN IF NOT EXISTS exchange_rate numeric(18,8),
    ADD COLUMN IF NOT EXISTS rate_date timestamptz;

-- ONE external ref = ONE deal per workspace. Partial, because console deals carry no ref.
CREATE UNIQUE INDEX IF NOT EXISTS uq_opportunities_tenant_external_ref
    ON opportunities (tenant_id, external_ref)
    WHERE external_ref IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_opportunities_owner
    ON opportunities (tenant_id, owner_id);
CREATE INDEX IF NOT EXISTS idx_opportunities_last_activity
    ON opportunities (tenant_id, last_activity_at);
CREATE INDEX IF NOT EXISTS idx_opportunities_source
    ON opportunities (tenant_id, source);
-- Every money read groups by currency; without this index the grouping is a table scan.
CREATE INDEX IF NOT EXISTS idx_opportunities_currency
    ON opportunities (tenant_id, pipeline_id, currency);

-- A workspace whose deals already agree on ONE currency adopts it as its default. This is a
-- MEASURED fact, not an assumption: a workspace whose deals disagree gets NO default, and its
-- next deal must state a currency (or the workspace must choose one).
UPDATE tenants t
   SET settings = jsonb_set(
        COALESCE(t.settings, '{}'::jsonb),
        '{default_currency}',
        to_jsonb(c.cur)
   )
  FROM (
        SELECT tenant_id, min(currency) AS cur
          FROM opportunities
         WHERE currency IS NOT NULL AND btrim(currency) <> ''
         GROUP BY tenant_id
        HAVING count(DISTINCT upper(currency)) = 1
       ) c
 WHERE t.id = c.tenant_id
   AND COALESCE(t.settings ->> 'default_currency', '') = '';

CREATE TABLE IF NOT EXISTS deal_collaborators (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id      uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    opportunity_id uuid NOT NULL REFERENCES opportunities(id) ON DELETE CASCADE,
    user_id        uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role           text NOT NULL DEFAULT 'collaborator',
    created_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (opportunity_id, user_id)
);
CREATE INDEX IF NOT EXISTS idx_deal_collaborators_tenant
    ON deal_collaborators (tenant_id, user_id);

CREATE TABLE IF NOT EXISTS deal_lost_reasons (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id  uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    label      text NOT NULL,
    position   integer NOT NULL DEFAULT 0,
    is_active  boolean NOT NULL DEFAULT true,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, label)
);

-- Every workspace starts with a usable list; it is editable per account afterwards.
INSERT INTO deal_lost_reasons (tenant_id, label, position)
SELECT t.id, d.label, d.position
  FROM tenants t
 CROSS JOIN (VALUES
        ('Price', 10),
        ('Timing', 20),
        ('No budget', 30),
        ('Chose a competitor', 40),
        ('No response', 50),
        ('Not a fit', 60),
        ('Other', 99)
     ) AS d(label, position)
ON CONFLICT (tenant_id, label) DO NOTHING;

CREATE TABLE IF NOT EXISTS deal_custom_fields (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id  uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    key        text NOT NULL,
    label      text NOT NULL,
    field_type text NOT NULL DEFAULT 'text',
    options    jsonb NOT NULL DEFAULT '[]'::jsonb,
    position   integer NOT NULL DEFAULT 0,
    is_active  boolean NOT NULL DEFAULT true,
    source_app text NOT NULL DEFAULT 'console',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, key)
);

CREATE TABLE IF NOT EXISTS deal_field_values (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    opportunity_id uuid NOT NULL REFERENCES opportunities(id) ON DELETE CASCADE,
    field_id       uuid NOT NULL REFERENCES deal_custom_fields(id) ON DELETE CASCADE,
    value          text,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (opportunity_id, field_id)
);

CREATE TABLE IF NOT EXISTS deal_activities (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id      uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    opportunity_id uuid NOT NULL REFERENCES opportunities(id) ON DELETE CASCADE,
    kind           text NOT NULL,
    body           text,
    actor_id       uuid REFERENCES users(id) ON DELETE SET NULL,
    actor_label    text,
    metadata       jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_deal_activities_deal
    ON deal_activities (tenant_id, opportunity_id, created_at DESC);

CREATE TABLE IF NOT EXISTS deal_tasks (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id      uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    opportunity_id uuid NOT NULL REFERENCES opportunities(id) ON DELETE CASCADE,
    title          text NOT NULL,
    notes          text,
    due_date       date,
    assigned_to    uuid REFERENCES users(id) ON DELETE SET NULL,
    completed_at   timestamptz,
    created_by     uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_deal_tasks_deal
    ON deal_tasks (tenant_id, opportunity_id, due_date);

CREATE TABLE IF NOT EXISTS deal_attachments (
    id             uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id      uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    opportunity_id uuid NOT NULL REFERENCES opportunities(id) ON DELETE CASCADE,
    filename       text NOT NULL,
    content_type   text NOT NULL DEFAULT 'application/octet-stream',
    size_bytes     integer NOT NULL,
    content        bytea NOT NULL,
    uploaded_by    uuid REFERENCES users(id) ON DELETE SET NULL,
    created_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_deal_attachments_deal
    ON deal_attachments (tenant_id, opportunity_id, created_at DESC);

CREATE TABLE IF NOT EXISTS deal_stage_automations (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    pipeline_id uuid NOT NULL REFERENCES pipelines(id) ON DELETE CASCADE,
    stage_id    uuid NOT NULL REFERENCES pipeline_stages(id) ON DELETE CASCADE,
    -- 'tag'      = apply this tag to the deal when it enters the stage
    -- 'workflow' = run the workspace's StageChanged automation rules (the Follow-Up tie-in)
    action_kind text NOT NULL,
    tag_id      uuid REFERENCES tags(id) ON DELETE CASCADE,
    is_active   boolean NOT NULL DEFAULT true,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT deal_stage_automations_kind CHECK (
        action_kind IN ('tag', 'workflow')
        AND (action_kind <> 'tag' OR tag_id IS NOT NULL)
    )
);
CREATE INDEX IF NOT EXISTS idx_deal_stage_automations_stage
    ON deal_stage_automations (tenant_id, stage_id);

-- Existing deals get a sensible activity stamp so "rotting" is not a false positive on day one.
UPDATE opportunities SET last_activity_at = updated_at WHERE last_activity_at > updated_at;
