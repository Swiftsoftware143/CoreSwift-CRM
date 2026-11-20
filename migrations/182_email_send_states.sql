-- The send log learns two more honest states.
--
-- 181 gave `email_message_log` three outcomes: sent, failed, refused_suppressed. The isolation work
-- (2026-11-19) adds two refusals that are neither: a message stopped because the account has spent
-- its allowance for this class of mail, and a message stopped because that account's OWN reputation
-- (bounces or spam complaints) paused its bulk lane. A row that was never sent must say WHY it was
-- never sent — folding either of these into "failed" would report a deliberate refusal as a fault,
-- and folding them into "refused_suppressed" would blame an opt-out that never happened.
--
-- Additive to a table created in the same working session: the constraint is widened, nothing is
-- dropped and no existing row's meaning changes.

ALTER TABLE email_message_log DROP CONSTRAINT IF EXISTS email_message_log_status_check;

ALTER TABLE email_message_log
    ADD CONSTRAINT email_message_log_status_check
    CHECK (status IN ('sent', 'failed', 'refused_suppressed', 'refused_quota', 'refused_reputation'));

-- The allowance is counted from this table, so it gets the index that count needs.
CREATE INDEX IF NOT EXISTS idx_email_message_log_class_window
    ON email_message_log (tenant_id, mail_class, created_at DESC);
