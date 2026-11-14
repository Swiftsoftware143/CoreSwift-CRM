-- Helpdesk core — the foundation Phase 1 stands on.
--
-- David, 2026-11-14. The Helpdesk replaces Freshdesk, so the ticket has to become a real unit of work
-- rather than a row a mail arrives in. This migration does four things and no more:
--
--   1. Widens the two vocabularies the product already enforces, moving the rows that hold the old
--      words in the SAME transaction (a CHECK added without the update leaves every existing row
--      violating it, so the next write fails on data that was legal when it was written).
--   2. Gives every ticket a human-friendly number, per account, allocated by a trigger so no creation
--      path can forget it — there are five places that insert a ticket today and five more coming.
--   3. Gives a ticket message a KIND, which is what makes an internal note possible: a note is never
--      sent, and until now every entry on a ticket was a reply by construction.
--   4. Carries the fields threading needs (the mail Message-Id and the id it replied to), so an
--      inbound reply can be matched onto its ticket instead of opening a new one.
--
-- Deliberately NOT here: queues, knowledge base, SLAs, CSAT, attachments, automations. Each lands with
-- the feature that reads it, so no column sits unused pretending to be implemented.

-- ── 1a. status: open/in_progress/resolved/closed -> the helpdesk's six ───────────────────────────
ALTER TABLE tickets DROP CONSTRAINT IF EXISTS tickets_status_check;
UPDATE tickets
   SET status = CASE status
                  WHEN 'in_progress' THEN 'open'
                  WHEN 'resolved'    THEN 'solved'
                  ELSE status
                END
 WHERE status IN ('in_progress', 'resolved');
ALTER TABLE tickets
  ADD CONSTRAINT tickets_status_check
  CHECK (status IN ('new', 'open', 'pending', 'on_hold', 'solved', 'closed'));
ALTER TABLE tickets ALTER COLUMN status SET DEFAULT 'new';

-- ── 1b. source: the helpdesk adds two channels, and keeps the four already in use ───────────────
ALTER TABLE tickets DROP CONSTRAINT IF EXISTS tickets_source_check;
ALTER TABLE tickets
  ADD CONSTRAINT tickets_source_check
  CHECK (source IN ('manual', 'email', 'form', 'agent', 'portal', 'widget', 'api'));

-- ── 1c. message authors widen: the AI and the system have to be distinguishable from a person ───
ALTER TABLE ticket_messages DROP CONSTRAINT IF EXISTS ticket_messages_sender_type_check;
ALTER TABLE ticket_messages
  ADD CONSTRAINT ticket_messages_sender_type_check
  CHECK (sender_type IN ('agent', 'contact', 'ai', 'system'));

-- ── 2. the ticket number, per account, from a trigger ───────────────────────────────────────────
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS number integer;

CREATE TABLE IF NOT EXISTS helpdesk_ticket_counters (
    tenant_id   uuid PRIMARY KEY,
    next_number integer NOT NULL DEFAULT 1
);

-- Existing tickets are numbered by age within their account, then the counters are set past the highest
-- number so the next ticket continues the sequence instead of colliding with history.
WITH numbered AS (
    SELECT id,
           ROW_NUMBER() OVER (
               PARTITION BY COALESCE(tenant_id, '00000000-0000-0000-0000-000000000000'::uuid)
               ORDER BY created_at, id
           ) AS rn
      FROM tickets
     WHERE number IS NULL
)
UPDATE tickets t
   SET number = n.rn
  FROM numbered n
 WHERE t.id = n.id;

INSERT INTO helpdesk_ticket_counters (tenant_id, next_number)
SELECT COALESCE(tenant_id, '00000000-0000-0000-0000-000000000000'::uuid), MAX(number) + 1
  FROM tickets
 WHERE number IS NOT NULL
 GROUP BY 1
ON CONFLICT (tenant_id) DO UPDATE
   SET next_number = GREATEST(helpdesk_ticket_counters.next_number, EXCLUDED.next_number);

-- The counter row is taken with an upsert, so two tickets created in the same instant serialise on the
-- row instead of both reading the same number. `next_number - 1` is the allocation: a fresh row starts
-- at 2 and hands out 1.
CREATE OR REPLACE FUNCTION helpdesk_assign_ticket_number() RETURNS trigger AS $$
BEGIN
    IF NEW.number IS NULL THEN
        INSERT INTO helpdesk_ticket_counters (tenant_id, next_number)
        VALUES (COALESCE(NEW.tenant_id, '00000000-0000-0000-0000-000000000000'::uuid), 2)
        ON CONFLICT (tenant_id) DO UPDATE
           SET next_number = helpdesk_ticket_counters.next_number + 1
        RETURNING next_number - 1 INTO NEW.number;
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_helpdesk_ticket_number ON tickets;
CREATE TRIGGER trg_helpdesk_ticket_number
    BEFORE INSERT ON tickets
    FOR EACH ROW EXECUTE FUNCTION helpdesk_assign_ticket_number();

-- Numbers already handed out must not repeat. The coalesce is because a ticket filed from the public
-- contact page belongs to no account yet (tenant_id NULL) and still needs a unique number.
CREATE UNIQUE INDEX IF NOT EXISTS idx_tickets_tenant_number
    ON tickets (COALESCE(tenant_id, '00000000-0000-0000-0000-000000000000'::uuid), number)
    WHERE number IS NOT NULL;

-- ── 3. the timeline: a note is not a reply ──────────────────────────────────────────────────────
ALTER TABLE ticket_messages ADD COLUMN IF NOT EXISTS kind text NOT NULL DEFAULT 'reply';
ALTER TABLE ticket_messages DROP CONSTRAINT IF EXISTS ticket_messages_kind_check;
ALTER TABLE ticket_messages
  ADD CONSTRAINT ticket_messages_kind_check
  CHECK (kind IN ('reply', 'note', 'event'));

ALTER TABLE ticket_messages ADD COLUMN IF NOT EXISTS body_text text;
ALTER TABLE ticket_messages ADD COLUMN IF NOT EXISTS body_html text;
ALTER TABLE ticket_messages ADD COLUMN IF NOT EXISTS author_id uuid;
ALTER TABLE ticket_messages ADD COLUMN IF NOT EXISTS edit_of uuid;

-- ── 4. threading: what an inbound reply is matched against ──────────────────────────────────────
ALTER TABLE ticket_messages ADD COLUMN IF NOT EXISTS email_message_id text;
ALTER TABLE ticket_messages ADD COLUMN IF NOT EXISTS in_reply_to text;
-- named email_references because `references` is a reserved word and every read would need quoting
ALTER TABLE ticket_messages ADD COLUMN IF NOT EXISTS email_references jsonb;
ALTER TABLE ticket_messages ADD COLUMN IF NOT EXISTS ai_confidence numeric(3, 2);
ALTER TABLE ticket_messages ADD COLUMN IF NOT EXISTS ai_sources jsonb;

-- The two lookups threading performs on every inbound message.
CREATE INDEX IF NOT EXISTS idx_ticket_messages_email_message_id
    ON ticket_messages (email_message_id) WHERE email_message_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_ticket_messages_in_reply_to
    ON ticket_messages (in_reply_to) WHERE in_reply_to IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_ticket_messages_ticket_created
    ON ticket_messages (ticket_id, created_at);

-- ── 5. the rest of the ticket's working fields ─────────────────────────────────────────────────
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS queue_id uuid;
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS ai_state text NOT NULL DEFAULT 'none';
ALTER TABLE tickets DROP CONSTRAINT IF EXISTS tickets_ai_state_check;
ALTER TABLE tickets
  ADD CONSTRAINT tickets_ai_state_check
  CHECK (ai_state IN ('none', 'ai_active', 'handed_off', 'ai_resolved'));
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS first_responded_at timestamptz;
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS solved_at timestamptz;
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS merged_into_id uuid REFERENCES tickets(id) ON DELETE SET NULL;
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS csat_score smallint;
ALTER TABLE tickets ADD COLUMN IF NOT EXISTS custom_fields jsonb NOT NULL DEFAULT '{}'::jsonb;

CREATE INDEX IF NOT EXISTS idx_tickets_tenant_status ON tickets (tenant_id, status);
CREATE INDEX IF NOT EXISTS idx_tickets_tenant_number_lookup ON tickets (tenant_id, number);
CREATE INDEX IF NOT EXISTS idx_tickets_open_assigned ON tickets (tenant_id, assigned_to)
    WHERE status IN ('new', 'open', 'pending', 'on_hold');

-- ── 6. the site: one brand per account, which is what a helpdesk address hangs off ─────────────
-- Phase 1 maps the spec's site onto the account (see docs/helpdesk-mapping.md): the only isolation
-- boundary the product has today is tenant_id, and every account of David's is already one brand.
-- A second brand under one account later becomes an added column here, not a new boundary.
CREATE TABLE IF NOT EXISTS helpdesk_sites (
    id               uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id        uuid NOT NULL UNIQUE REFERENCES tenants(id) ON DELETE CASCADE,
    name             text NOT NULL,
    slug             text NOT NULL UNIQUE,
    support_email    text,
    from_name        text,
    brand            jsonb NOT NULL DEFAULT '{}'::jsonb,
    business_hours   jsonb NOT NULL DEFAULT '{}'::jsonb,
    allowed_origins  text[] NOT NULL DEFAULT '{}',
    ai_enabled       boolean NOT NULL DEFAULT false,
    ai_persona       text,
    default_queue_id uuid,
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now()
);

-- Every existing account gets its site row now, so no ticket can arrive at an account with no brand
-- to render it under.
INSERT INTO helpdesk_sites (tenant_id, name, slug)
SELECT t.id, t.name, t.slug
  FROM tenants t
 WHERE NOT EXISTS (SELECT 1 FROM helpdesk_sites s WHERE s.tenant_id = t.id);

CREATE TABLE IF NOT EXISTS helpdesk_queues (
    id                 uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id          uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name               text NOT NULL,
    members            uuid[] NOT NULL DEFAULT '{}',
    round_robin_cursor integer NOT NULL DEFAULT 0,
    created_at         timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, name)
);

CREATE INDEX IF NOT EXISTS idx_tickets_tenant_queue ON tickets (tenant_id, queue_id);
