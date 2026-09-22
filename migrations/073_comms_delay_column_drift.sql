-- 073: two coreswift_crm tables drifted from the shape migration 067 created for them.
-- The live tables were created before 067 ran, so `CREATE TABLE IF NOT EXISTS` skipped them
-- and the columns 067 defines were never added. Eight call sites name columns that do not
-- exist, so every write raises 42703 / every decode raises "no column found for name".
--
-- Evidence (kanban t_43201f56, verified 2026-09-22 UTC against the live db and container):
--   message_templates.updated_at  absent -> GET /api/comms/templates 500s as soon as one row
--       exists (14 rows live). Log: `crm_swift::errors: Database error
--       error=no column found for name: updated_at`. Empty tenants answered 200, which is
--       why it went unnoticed. POST/PATCH also 500.
--   delayed_actions absent: trigger_event_id, condition_type, condition_config, action_config,
--       result, updated_at, entity_type, entity_id -> 8 write sites fail today
--       (src/worker.rs:356,476, src/monitoring/engine.rs:100,
--       src/monitoring/account_health_handler.rs:138,202,370, src/checklists/engine.rs:57,
--       src/events/handlers.rs:238, src/private_email/auto_reply_handler.rs:250), and
--       src/events/handlers.rs:266 / src/events/dispatcher.rs:288 UPDATE updated_at.
--       The table is empty (0 rows) precisely because no insert has ever succeeded, so the
--       "If-Not-Then" delayed-action engine has never run in production.
--       Proof: BEGIN READ ONLY; EXPLAIN INSERT INTO delayed_actions (..., trigger_event_id, ...)
--       -> ERROR: column "trigger_event_id" of relation "delayed_actions" does not exist.
--
-- Additive only: no column is dropped or retyped. The live table's extra columns
-- (payload, status, error_message) stay — payload is used by the auto-reply writer.
-- Columns that some writers omit (condition_type, trigger_event_id, entity_type, entity_id)
-- are intentionally nullable, where 067 declared condition_type NOT NULL: making them NOT NULL
-- here would keep src/private_email/auto_reply_handler.rs:250 broken.

ALTER TABLE message_templates
    ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ;

UPDATE message_templates SET updated_at = created_at WHERE updated_at IS NULL;

ALTER TABLE message_templates ALTER COLUMN updated_at SET DEFAULT NOW();
ALTER TABLE message_templates ALTER COLUMN updated_at SET NOT NULL;

ALTER TABLE delayed_actions ADD COLUMN IF NOT EXISTS trigger_event_id UUID;
ALTER TABLE delayed_actions ADD COLUMN IF NOT EXISTS condition_type VARCHAR(20);
ALTER TABLE delayed_actions ADD COLUMN IF NOT EXISTS condition_config JSONB NOT NULL DEFAULT '{}'::jsonb;
ALTER TABLE delayed_actions ADD COLUMN IF NOT EXISTS action_config JSONB NOT NULL DEFAULT '{}'::jsonb;
ALTER TABLE delayed_actions ADD COLUMN IF NOT EXISTS result JSONB;
ALTER TABLE delayed_actions ADD COLUMN IF NOT EXISTS entity_type VARCHAR(50);
ALTER TABLE delayed_actions ADD COLUMN IF NOT EXISTS entity_id UUID;
ALTER TABLE delayed_actions ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW();

-- 067 also declared the events FK and two indexes; re-add whatever is missing.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint
        WHERE conname = 'delayed_actions_trigger_event_id_fkey'
          AND conrelid = 'delayed_actions'::regclass
    ) THEN
        ALTER TABLE delayed_actions
            ADD CONSTRAINT delayed_actions_trigger_event_id_fkey
            FOREIGN KEY (trigger_event_id) REFERENCES events(id) ON DELETE SET NULL;
    END IF;
END $$;

CREATE INDEX IF NOT EXISTS idx_delayed_actions_tenant ON delayed_actions(tenant_id);
CREATE INDEX IF NOT EXISTS idx_delayed_actions_execute
    ON delayed_actions(tenant_id, execute_at) WHERE executed = false AND cancelled = false;
CREATE INDEX IF NOT EXISTS idx_delayed_actions_entity ON delayed_actions(tenant_id, entity_type, entity_id);
