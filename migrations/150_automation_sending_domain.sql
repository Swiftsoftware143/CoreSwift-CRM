-- 150 — per-account automation sending subdomain (kanban t_044d5d62).
--
-- David's standing rule: PRIVATE MAIL and AUTOMATION run on SEPARATE lanes, so a complaint about an
-- automated blast cannot damage the domain real conversations arrive on. The private mailbox sends
-- from the account's MAIN domain (warm, human replies); automation (auto-responders, sequences,
-- campaigns, the send_email rule action) must send from a SUBDOMAIN of that same domain —
-- `mg.<domain>` — and a reply to it must route back to the account's private inbox.
--
-- The queue is the shared delivery path (`worker::deliver_queued_messages`), so a queued message has
-- to SAY which lane it belongs to. A NULL `automation_domain` means "ordinary mail" (the existing
-- resolution: the account's main domain, else the platform transport). A non-NULL value is the
-- automation lane's full sending subdomain (e.g. `mg.acme.com`), and the worker resolves its
-- transport from the account's own Mailgun credential for that domain — never the main-domain lane.
ALTER TABLE outbound_messages
    ADD COLUMN IF NOT EXISTS automation_domain text;

COMMENT ON COLUMN outbound_messages.automation_domain IS
    'The account''s automation sending subdomain (mg.<main-domain>, kanban t_044d5d62). NULL = ordinary mail (main-domain lane); non-NULL = automation lane.';
