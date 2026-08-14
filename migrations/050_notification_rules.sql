-- 050_notification_rules.sql
-- Notification Rules Engine — auto-fire comms on pipeline/score/booking events.
-- IDEMPOTENT: the live DB had a divergent notification_rules table
-- (event_type/channel instead of trigger_event/action) and was empty.
-- This reconciles the table to the code-expected schema by adding the missing
-- columns, and it guards the index/constraint operations.

-- Add missing columns to the existing notification_rules table (code expects these).
ALTER TABLE notification_rules ADD COLUMN IF NOT EXISTS trigger_event TEXT;
ALTER TABLE notification_rules ADD COLUMN IF NOT EXISTS action TEXT;
ALTER TABLE notification_rules ADD COLUMN IF NOT EXISTS target_entity TEXT;
ALTER TABLE notification_rules ADD COLUMN IF NOT EXISTS config JSONB DEFAULT '{}';
ALTER TABLE notification_rules ADD COLUMN IF NOT EXISTS updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

-- Backfill trigger_event/action from the legacy columns if they were non-empty
UPDATE notification_rules SET trigger_event = event_type WHERE trigger_event IS NULL AND event_type IS NOT NULL;
UPDATE notification_rules SET action = channel WHERE action IS NULL AND channel IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_notification_rules_tenant ON notification_rules(tenant_id);
CREATE INDEX IF NOT EXISTS idx_notification_rules_trigger ON notification_rules(tenant_id, trigger_event) WHERE is_active = true;

-- Queue for background delivery (guarded: table may already exist with a superset schema)
CREATE TABLE IF NOT EXISTS notification_queue (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    rule_id UUID REFERENCES notification_rules(id) ON DELETE SET NULL,
    channel TEXT NOT NULL,
    to_address TEXT,
    subject TEXT,
    body TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued',
    error_message TEXT,
    sent_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_notification_queue_tenant ON notification_queue(tenant_id);
CREATE INDEX IF NOT EXISTS idx_notification_queue_status ON notification_queue(status) WHERE status = 'queued';

-- Update outbound_messages channel constraint to include whatsapp (guarded).
DO $$
DECLARE
    has_whatsapp BOOLEAN;
BEGIN
    SELECT pg_get_constraintdef(oid) LIKE '%whatsapp%' INTO has_whatsapp
    FROM pg_constraint
    WHERE conrelid = 'outbound_messages'::regclass AND conname = 'outbound_messages_channel_check';
    IF has_whatsapp IS NOT TRUE THEN
        ALTER TABLE outbound_messages DROP CONSTRAINT IF EXISTS outbound_messages_channel_check;
        ALTER TABLE outbound_messages ADD CONSTRAINT outbound_messages_channel_check CHECK (channel IN ('email', 'sms', 'whatsapp'));
    END IF;
END $$;

-- Update message_templates channel constraint to include whatsapp (guarded).
DO $$
DECLARE
    has_whatsapp BOOLEAN;
BEGIN
    SELECT pg_get_constraintdef(oid) LIKE '%whatsapp%' INTO has_whatsapp
    FROM pg_constraint
    WHERE conrelid = 'message_templates'::regclass AND conname = 'message_templates_channel_check';
    IF has_whatsapp IS NOT TRUE THEN
        ALTER TABLE message_templates DROP CONSTRAINT IF EXISTS message_templates_channel_check;
        ALTER TABLE message_templates ADD CONSTRAINT message_templates_channel_check CHECK (channel IN ('email', 'sms', 'whatsapp'));
    END IF;
END $$;
