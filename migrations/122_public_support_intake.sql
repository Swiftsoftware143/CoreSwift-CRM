-- 122_public_support_intake.sql — a visitor who cannot sign in can reach the team.
--
-- MEASURED 2026-11-06 (dns.google and cloudflare-dns.com agree): MX = 0 for coreswiftcrm.com,
-- swiftsoftware.net and every sibling domain, and nothing on this host listens on port 25. Every
-- "write to support@…" sentence this app prints — the login page, the marketing site, the guide and
-- the legal pages — therefore sent a customer's message into a bounce, and a locked-out customer had
-- no way back in.
--
-- The served contact page now files the message into the operator's existing ticket queue instead:
-- POST /api/public/support-request writes ONE ticket with no workspace (`tenant_id` NULL), the address
-- the visitor typed in `contact_email` (the column this app already uses to answer a form request) and
-- the visitor's words as the ticket body. A platform admin's ticket queue shows those rows labelled
-- "Website visitor", so the person who can help sees them in the screen they already work.

-- A visitor belongs to no workspace. The column keeps its foreign key, so every ticket filed from
-- inside a workspace still names its tenant; only the public arm leaves it empty.
ALTER TABLE tickets ALTER COLUMN tenant_id DROP NOT NULL;

-- The public arm counts recent messages from one address to refuse a fourth within the hour, and the
-- operator's queue shows the address to answer — both read lower(contact_email) on public rows only.
CREATE INDEX IF NOT EXISTS idx_tickets_public_visitor
    ON tickets (lower(contact_email))
    WHERE tenant_id IS NULL;
