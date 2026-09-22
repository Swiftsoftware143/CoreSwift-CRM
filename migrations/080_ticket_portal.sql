-- 080_ticket_portal.sql — CS-22: the customer-facing "💬 My Support" portal.
--
-- A request submitted through the portal is a customer self-service form, and it needs its own
-- `source` value: the existing CHECK only allowed manual|email|form|agent, and re-using 'form'
-- would make a customer's own submission indistinguishable from the tenant's staff form.
-- Widening a CHECK is backward-compatible — no row is rewritten, and an older binary inserting
-- one of the four original values is unaffected.

ALTER TABLE tickets DROP CONSTRAINT IF EXISTS tickets_source_check;
ALTER TABLE tickets ADD CONSTRAINT tickets_source_check
    CHECK (source = ANY (ARRAY['manual'::text, 'email'::text, 'form'::text, 'agent'::text, 'portal'::text]));
