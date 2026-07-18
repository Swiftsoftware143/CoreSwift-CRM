-- 050_notification_rules.sql
-- Notification Rules Engine — auto-fire comms on pipeline/score/booking events

-- Define trigger events and actions for notification rules
CREATE TABLE IF NOT EXISTS notification_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    trigger_event TEXT NOT NULL,                  -- pipeline_move|score_threshold|booking_created|booking_cancelled|custom
    action TEXT NOT NULL,                         -- send_email|send_sms|send_whatsapp|in_app
    template_id UUID REFERENCES message_templates(id) ON DELETE SET NULL,
    target_entity TEXT,                           -- contact|pipeline_owner|tenant_admins
    config JSONB DEFAULT '{}',                    -- score_threshold, pipeline_stage_id, etc.
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_notification_rules_tenant ON notification_rules(tenant_id);
CREATE INDEX IF NOT EXISTS idx_notification_rules_trigger ON notification_rules(tenant_id, trigger_event) WHERE is_active = true;

-- Queue for background delivery of notification-triggered messages
CREATE TABLE IF NOT EXISTS notification_queue (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    rule_id UUID REFERENCES notification_rules(id) ON DELETE SET NULL,
    channel TEXT NOT NULL,                        -- email|sms|whatsapp|in_app
    to_address TEXT,
    subject TEXT,
    body TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued',        -- queued|sending|sent|failed
    error_message TEXT,
    sent_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_notification_queue_tenant ON notification_queue(tenant_id);
CREATE INDEX IF NOT EXISTS idx_notification_queue_status ON notification_queue(status) WHERE status = 'queued';

-- Update the outbound_messages channel constraint to include whatsapp
ALTER TABLE outbound_messages DROP CONSTRAINT IF EXISTS outbound_messages_channel_check;
ALTER TABLE outbound_messages ADD CONSTRAINT outbound_messages_channel_check CHECK (channel IN ('email', 'sms', 'whatsapp'));

-- Update message_templates channel constraint to include whatsapp
ALTER TABLE message_templates DROP CONSTRAINT IF EXISTS message_templates_channel_check;
ALTER TABLE message_templates ADD CONSTRAINT message_templates_channel_check CHECK (channel IN ('email', 'sms', 'whatsapp'));
