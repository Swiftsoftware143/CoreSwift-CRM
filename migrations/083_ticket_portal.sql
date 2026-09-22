-- 083_ticket_portal.sql — CS-22: the customer-facing "💬 My Support" portal.
--
-- Version 083, not 080: `080_private_email_api_keys_sealed_guard.sql` already holds version 80, and
-- sqlx answers a duplicate version by logging "migration 80 was previously applied but has been
-- modified" and SKIPPING it — which is what happened to the first attempt at this migration (it also
-- made a genuine, already-applied migration look modified). 082 is claimed in-flight by another
-- lane's email_templates migration in the shared tree, so take the next free number after it.
--
-- A request submitted through the portal is a customer self-service form, and it needs its own
-- `source` value: the existing CHECK only allowed manual|email|form|agent, and re-using 'form'
-- would make a customer's own submission indistinguishable from the tenant's staff form.
-- Widening a CHECK is backward-compatible — no row is rewritten, and an older binary inserting
-- one of the four original values is unaffected.

ALTER TABLE tickets DROP CONSTRAINT IF EXISTS tickets_source_check;
ALTER TABLE tickets ADD CONSTRAINT tickets_source_check
    CHECK (source = ANY (ARRAY['manual'::text, 'email'::text, 'form'::text, 'agent'::text, 'portal'::text]));
