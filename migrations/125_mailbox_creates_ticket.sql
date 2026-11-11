-- 125_mailbox_creates_ticket.sql
-- Which mailboxes turn incoming mail into a ticket automatically.
--
-- David 2026-11-11: support@ is the default work queue and ships ON; every other
-- mailbox ships OFF, and the user may tick or untick any of them. Mail ALWAYS
-- arrives in the inbox and is tagged with the address it came to - the toggle
-- decides what becomes work automatically, never what is kept.

ALTER TABLE private_email_boxes
    ADD COLUMN IF NOT EXISTS creates_ticket BOOLEAN NOT NULL DEFAULT false;

-- support@ starts as the work queue...
UPDATE private_email_boxes SET creates_ticket = true
 WHERE local_part = 'support' AND creates_ticket = false;

-- ...and whichever box an account already pointed at keeps creating tickets,
-- so nobody loses their current behaviour on upgrade.
UPDATE private_email_boxes b SET creates_ticket = true
  FROM tenants t
 WHERE (t.settings->>'support_email_box_id') = b.id::text
   AND b.creates_ticket = false;
