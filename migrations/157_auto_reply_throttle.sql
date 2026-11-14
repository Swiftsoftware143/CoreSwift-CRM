-- 157 — the auto-reply throttle (kanban: CoreSwift auto-replies had no door, and no brake).
--
-- The engine in `src/private_email/auto_reply_handler.rs` fires on every inbound delivery. Two
-- problems with letting a customer switch it on from a screen:
--   * nothing limited it, so a busy address sent a reply per message, and two rules pointing at
--     each other (or two of our own mailboxes) could exchange replies for ever;
--   * nothing recorded that a reply had already gone to a given person.
--
-- One row per (rule, recipient) is the whole state: the send path claims the row with a
-- conditional upsert, so the claim is atomic under concurrency and the window is exactly 24 hours.
-- No history is kept beyond the last claim — this is a brake, not an archive.

CREATE TABLE IF NOT EXISTS public.private_email_auto_reply_sent (
    rule_id   uuid        NOT NULL,
    recipient text        NOT NULL,
    sent_at   timestamptz NOT NULL DEFAULT NOW(),
    CONSTRAINT private_email_auto_reply_sent_pkey PRIMARY KEY (rule_id, recipient),
    CONSTRAINT private_email_auto_reply_sent_rule_fkey FOREIGN KEY (rule_id)
        REFERENCES public.private_email_auto_replies(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_private_email_auto_reply_sent_at
    ON public.private_email_auto_reply_sent (sent_at);
