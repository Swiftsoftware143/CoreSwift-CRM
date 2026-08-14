-- Migration 061: Support Widgets & Inboxes
-- Multi-widget support system — each widget is a named embeddable contact form
-- that routes submissions to a specific inbox. Gated by plan (max_widgets).

-- Inbox: a named support mailbox. One tenant can have many inboxes.
CREATE TABLE IF NOT EXISTS ticket_inboxes (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    email_fwd   TEXT, -- forward new tickets to this email
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX idx_ticket_inboxes_tenant ON ticket_inboxes(tenant_id);

-- Support widget: an embeddable support form tied to an inbox.
CREATE TABLE IF NOT EXISTS support_widgets (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    inbox_id    UUID NOT NULL REFERENCES ticket_inboxes(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,        -- "CoreSwift CRM", "FunnelSwift", etc.
    slug        TEXT NOT NULL,        -- unique per tenant, used in embed URL
    theme_color TEXT NOT NULL DEFAULT '#2563eb',
    greeting    TEXT NOT NULL DEFAULT 'How can we help?',
    welcome_msg TEXT NOT NULL DEFAULT 'Thanks for reaching out! We will get back to you shortly.',
    position    TEXT NOT NULL DEFAULT 'bottom-right' CHECK (position IN ('bottom-right','bottom-left')),
    is_active   BOOLEAN NOT NULL DEFAULT true,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(tenant_id, slug)
);
CREATE INDEX idx_support_widgets_tenant ON support_widgets(tenant_id);
CREATE INDEX idx_support_widgets_inbox ON support_widgets(inbox_id);

-- Add widget_id to tickets so we know which widget generated the ticket
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS widget_id UUID REFERENCES support_widgets(id) ON DELETE SET NULL;
CREATE INDEX idx_tickets_widget_id ON tickets(widget_id);
