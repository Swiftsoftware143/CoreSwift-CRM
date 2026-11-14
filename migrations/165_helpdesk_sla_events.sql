-- SLA events: the clock has to tell somebody.
--
-- Migration 163 stamped each ticket with the two deadlines it is measured against, and the ticket list
-- counts down to them. This records what HAPPENED — a warning before a deadline and a breach after it —
-- so the same event is never fired twice by a restart, a second instance or two ticks landing together.
--
-- The record is per (ticket, clock, kind) rather than a timestamp on the ticket for one reason: a ticket
-- can be answered late, reopened and breach again, and the second breach is a different fact from the
-- first. A flag on the row would have to be reset, and a flag nobody resets is a clock that stops.

CREATE TABLE IF NOT EXISTS helpdesk_sla_events (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id  uuid NOT NULL,
    ticket_id  uuid NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    -- which promise was missed: answering it, or resolving it
    clock      text NOT NULL CHECK (clock IN ('first_response', 'resolution')),
    kind       text NOT NULL CHECK (kind IN ('approaching', 'breached')),
    due_at     timestamptz NOT NULL,
    fired_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (ticket_id, clock, kind)
);

CREATE INDEX IF NOT EXISTS idx_helpdesk_sla_events_recent
    ON helpdesk_sla_events (tenant_id, fired_at DESC);
