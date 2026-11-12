-- 148_private_email_attachment_not_stored.sql
-- A file the receiver could NOT keep says so on the message (kanban t_0b3f3165).
--
-- 147 gave a message's file its own row. It also left two holes a customer would feel:
--   * no inbound size/count ceiling — one huge part was stored whatever it cost the account;
--   * the 100% rule was only enforced on the OUTBOUND door, so a file arriving at a full
--     mailbox was still stored, pushing usage further past the allowance it had just refused.
--
-- Inbound mail is still never refused and nothing is ever deleted. Instead a part that cannot be
-- kept (too large, too many, or no room left) gets a row WITHOUT bytes, with `size_bytes = 0`, so
-- the storage meter stays honest, and with the plain sentence the reader shows beside the filename.
-- A download of such a row is refused rather than answering an empty file.
--
-- `data` stays NOT NULL: a notice row stores the empty byte string, which is exactly what it holds.

ALTER TABLE private_email_attachments
    ADD COLUMN IF NOT EXISTS not_stored_reason text;

COMMENT ON COLUMN private_email_attachments.not_stored_reason IS
    'NULL when the bytes are stored. Otherwise the plain sentence explaining why this file was not saved (too large, too many, or the mailbox was full); such a row carries zero bytes.';

-- The reader lists a message's files through (tenant, message); notice rows are read the same way,
-- so the existing index already serves them. The inline lookup filters on size_bytes > 0 in code.
