-- 129_private_email_backfill_ticket_link.sql
--
-- The backfill in migration 127 rebuilt the inbox from the `events` table, which had recorded the
-- sender, recipient, subject and a body preview of every delivery that arrived before messages were
-- stored. It could not know which of those messages had ALSO become a ticket — that fact lived only
-- in the tickets table, with nothing linking the two.
--
-- Left alone, every such row would tell the user "Inbox only — nobody is working this yet" while a
-- ticket for it sat in the work queue. That is a false statement on screen, which is worse than a
-- missing one.
--
-- THE MATCH is deliberately narrow: same account, identical subject, and the ticket created within
-- a minute of the message arriving (measured: every genuine pair here is within 7 MILLISECONDS —
-- the ticket is written by the same webhook call). A wider window or a looser key would attach the
-- wrong ticket to the wrong mail, and this repair only ever sets a flag; it never removes one.
--
-- Idempotent: only rows that are not already a ticket are touched, so re-running changes nothing.

UPDATE private_email_messages m
   SET ticket_id = t.id,
       is_ticket = true,
       updated_at = NOW()
  FROM tickets t
 WHERE t.tenant_id = m.tenant_id
   AND t.subject = m.subject
   AND m.is_ticket = false
   AND m.ticket_id IS NULL
   AND abs(extract(epoch FROM (t.created_at - m.received_at))) <= 60;
