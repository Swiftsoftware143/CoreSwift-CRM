-- At-risk escalation (ticketing suite, item 2).
--
-- WHAT EXISTED: a minute worker that warned an hour before a deadline and recorded each breach, both as a
-- ledger row and as an event on the ticket timeline. So a promise that was about to be missed produced a
-- line in a queue — visible only to whoever was already looking at that ticket.
--
-- WHAT THIS ADDS: the ladder. An approaching deadline tells the person who owns the ticket; a missed one
-- tells the account's owner/admin as well, and is recorded once per rung so a slow week cannot turn into a
-- mail storm. Nothing here replaces the timeline events — they stay the record of what happened.
--
-- The rung is a (clock, rung) pair, so "first response due soon" and "first response missed" are separate
-- rungs, and the resolution clock climbs its own ladder independently.

CREATE TABLE IF NOT EXISTS ticket_escalations (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id    uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    ticket_id    uuid NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    clock        text NOT NULL,                     -- 'first_response' | 'resolution'
    rung         text NOT NULL,                     -- 'due_soon'    | 'breached'
    audience     text NOT NULL,                     -- 'assignee'    | 'owner'
    delivered    boolean NOT NULL DEFAULT false,    -- false = nobody was actually reachable
    detail       text,                              -- why it was not delivered, if it was not
    created_at   timestamptz NOT NULL DEFAULT now()
);

-- ONE TELLING PER TICKET PER CLOCK PER RUNG. The worker ticks every minute; without this a ticket stuck
-- over a weekend mails its owner 2,880 times.
CREATE UNIQUE INDEX IF NOT EXISTS uq_ticket_escalations_rung
    ON ticket_escalations (ticket_id, clock, rung, audience);

CREATE INDEX IF NOT EXISTS idx_ticket_escalations_tenant
    ON ticket_escalations (tenant_id, created_at DESC);

-- Sentiment on the ticket, so escalation can weigh tone as well as time. The values are deliberately few:
-- a tag nobody can act on is worse than no tag. Nullable, because "not judged yet" is a real state.
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS sentiment text;

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'tickets_sentiment_check'
    ) THEN
        ALTER TABLE tickets
            ADD CONSTRAINT tickets_sentiment_check
            CHECK (sentiment IS NULL OR sentiment IN ('calm', 'frustrated', 'angry'));
    END IF;
END $$;

-- The two messages the ladder sends. Seeded as DEFAULTS so every workspace has working wording without
-- configuring anything; a workspace that wants its own voice overrides them like any other template.
INSERT INTO email_templates (id, template_type, name, subject, body, is_default, created_at, updated_at)
SELECT gen_random_uuid(), 'ticket_due_soon', 'Ticket deadline approaching',
       'A ticket you own is close to its deadline',
       E'{{number}} is due {{due_at}} and has not been answered yet.\n\nSubject: {{subject}}\n\nOpen it: {{link}}\n',
       true, now(), now()
WHERE NOT EXISTS (SELECT 1 FROM email_templates WHERE template_type = 'ticket_due_soon');

INSERT INTO email_templates (id, template_type, name, subject, body, is_default, created_at, updated_at)
SELECT gen_random_uuid(), 'ticket_sla_breached', 'Ticket deadline missed',
       'A promised deadline was missed on {{number}}',
       E'{{number}} missed its {{clock}} promise (due {{due_at}}) and is still open.\n\nSubject: {{subject}}\n\nOpen it: {{link}}\n',
       true, now(), now()
WHERE NOT EXISTS (SELECT 1 FROM email_templates WHERE template_type = 'ticket_sla_breached');
