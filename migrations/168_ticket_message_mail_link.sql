-- The files a customer sent, on the ticket they sent them to.
--
-- Attachments already arrive: the mail pipeline stores them in `private_email_attachments` with the bytes,
-- and the shared inbox can show them. What was missing is the LINK — a ticket message did not know which
-- stored mail it came from, so the ticket's timeline could not show that a screenshot had been sent, and an
-- agent reading a ticket about a broken invoice saw prose and no file.
--
-- The link is a column rather than a join on the Message-Id header because the header is not a key: a
-- customer's client can repeat one, and a missing header would break the join silently. Ingest knows the row
-- it just stored, so it records it.

ALTER TABLE ticket_messages
    ADD COLUMN IF NOT EXISTS email_message_row_id uuid
    REFERENCES private_email_messages(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_ticket_messages_email_row
    ON ticket_messages (email_message_row_id)
    WHERE email_message_row_id IS NOT NULL;

-- Backfill what can be linked honestly: an exact Message-Id match inside the same ticket's mail. Anything
-- ambiguous stays NULL rather than being guessed at, because a wrong file on a ticket is worse than none.
UPDATE ticket_messages tm
   SET email_message_row_id = pm.id
  FROM private_email_messages pm
 WHERE tm.email_message_row_id IS NULL
   AND tm.email_message_id IS NOT NULL
   AND pm.message_id = tm.email_message_id
   AND pm.ticket_id = tm.ticket_id;
