-- A support desk that remembers.
--
-- The same question arrives twice, three times, fifty times. Each time somebody answers it well and the answer
-- then disappears into a closed ticket. This is the ledger that stops that: a solved ticket becomes a CANDIDATE
-- article, drafted automatically, and published only when a person reads it.
--
-- The ledger exists rather than a flag on the ticket for the same reason the SLA events do: a ticket can be
-- solved, reopened and solved again, and the second answer may be worth harvesting when the first was not. One
-- row per TICKET (`UNIQUE`) is deliberate — a ticket is harvested once, or not at all, so the queue of drafts
-- cannot fill with copies of the same conversation.
--
-- `outcome` is what makes this "self-learning" rather than "auto-generating": published and discarded are both
-- recorded, so the ratio per account is a fact rather than a feeling, and a future rule can lean on it.

CREATE TABLE IF NOT EXISTS helpdesk_kb_learning (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    ticket_id   uuid NOT NULL REFERENCES tickets(id) ON DELETE CASCADE,
    article_id  uuid REFERENCES helpdesk_kb_articles(id) ON DELETE SET NULL,
    status      text NOT NULL DEFAULT 'queued'
                CHECK (status IN ('queued', 'drafted', 'published', 'discarded', 'skipped')),
    -- why it was skipped, in words a person can read back later
    reason      text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    resolved_at timestamptz,
    UNIQUE (ticket_id)
);

CREATE INDEX IF NOT EXISTS idx_helpdesk_kb_learning_queue
    ON helpdesk_kb_learning (status, created_at)
    WHERE status = 'queued';

-- Which article came from which conversation, and that it was a machine's first attempt rather than a person's
-- words. The console shows both, and an owner reading an old article can see where it came from.
ALTER TABLE helpdesk_kb_articles ADD COLUMN IF NOT EXISTS source_ticket_id uuid
    REFERENCES tickets(id) ON DELETE SET NULL;
ALTER TABLE helpdesk_kb_articles ADD COLUMN IF NOT EXISTS auto_drafted boolean NOT NULL DEFAULT false;

-- Per account, switchable: an owner who does not want suggestions turns them off and nothing is queued.
ALTER TABLE helpdesk_sites ADD COLUMN IF NOT EXISTS kb_autodraft boolean NOT NULL DEFAULT true;
