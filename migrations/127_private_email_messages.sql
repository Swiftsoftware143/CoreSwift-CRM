-- 127_private_email_messages.sql
--
-- THE MISSING HALF OF RECEIVING MAIL (kanban B204/B205/B211).
--
-- Until this migration the app had no record of a received message. The inbound webhook
-- authenticated the delivery, found or created a CONTACT, and then decided whether to write a
-- TICKET (private_email_boxes.creates_ticket, migration 125). A message that did not become a
-- ticket was therefore recorded nowhere a human could see it: mail to info@ was accepted by the
-- provider, acknowledged with 200, and vanished. support@ looked fine only because it happened to
-- create a ticket, and the inbox was reading tickets — so "mail arrives" and "mail is visible"
-- were two different features wearing one name.
--
-- A message is its own object now. Every authenticated inbound delivery gets exactly one row here,
-- whatever the ticket toggle says, tagged with the ADDRESS it arrived on and flagged whether it
-- ALSO became a ticket. The inbox reads this table; the work queue still reads tickets; one mail
-- can appear in both, and nothing is dropped.
--
-- ACCOUNT LEVEL, NOT WORKSPACE LEVEL (B211, David 2026-11-11): there is no workspace_id column and
-- there must not be one. A workspace organises CRM data. Mail belongs to the business account.

CREATE TABLE IF NOT EXISTS private_email_messages (
    id                  uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id           uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    -- The mailbox the message is filed under. NULL means it arrived at an address nobody created
    -- and was accepted by the domain's catch-all — the MISC bucket in the address filter (B211).
    -- ON DELETE SET NULL, never CASCADE: deleting a mailbox must not destroy the mail that arrived
    -- through it, and `address` below still says where it came from.
    mailbox_id          uuid REFERENCES private_email_boxes(id) ON DELETE SET NULL,
    -- The address the message ARRIVED ON, verbatim and lowercased. This is what makes the address
    -- filter possible: one catch-all inbox can receive for many addresses (B211).
    address             varchar(320) NOT NULL,
    -- Which store the message sits in. The eight surfaces the user sees (Inbox, Unread, Favorites,
    -- Drafts, Sent, Archive, Spam, Trash) are VIEWS over this column plus is_read/is_starred;
    -- only a real store is written here. A custom subfolder is a path below its parent, e.g.
    -- 'inbox/Clients' (B205 "Create subfolder").
    folder              varchar(64) NOT NULL DEFAULT 'inbox',
    direction           varchar(8) NOT NULL DEFAULT 'inbound',
    from_address        varchar(320) NOT NULL DEFAULT '',
    from_name           varchar(200),
    -- Every recipient as the provider delivered it (to + cc), so reply-all can rebuild the list.
    to_address          text NOT NULL DEFAULT '',
    cc_address          text,
    subject             text NOT NULL DEFAULT '',
    body_plain          text NOT NULL DEFAULT '',
    body_html           text,
    -- First ~500 characters of the plain body, for the list view without shipping whole bodies.
    snippet             text NOT NULL DEFAULT '',
    message_id          text,
    in_reply_to         text,
    provider            varchar(32),
    provider_message_id text,
    is_read             boolean NOT NULL DEFAULT false,
    is_starred          boolean NOT NULL DEFAULT false,
    size_bytes          integer NOT NULL DEFAULT 0,
    has_attachments     boolean NOT NULL DEFAULT false,
    -- The ticket this message also created (or was turned into by hand). NULL = inbox only.
    ticket_id           uuid REFERENCES tickets(id) ON DELETE SET NULL,
    is_ticket           boolean NOT NULL DEFAULT false,
    -- "assign to a team member" (B204/B205) — the same object as ticket assignment.
    assigned_user_id    uuid REFERENCES users(id) ON DELETE SET NULL,
    received_at         timestamptz NOT NULL DEFAULT now(),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);

-- Idempotency: a provider retries a delivery it did not see acknowledged. The Message-Id is the
-- sender's own identifier for the message, so (tenant, message_id) is the natural guard — a
-- retried webhook updates nothing and creates no second row. Partial, because not every provider
-- sends one and an empty string must never collide.
CREATE UNIQUE INDEX IF NOT EXISTS idx_private_email_messages_dedupe
    ON private_email_messages (tenant_id, message_id)
    WHERE message_id IS NOT NULL AND message_id <> '';

CREATE INDEX IF NOT EXISTS idx_private_email_messages_inbox
    ON private_email_messages (tenant_id, folder, received_at DESC);
CREATE INDEX IF NOT EXISTS idx_private_email_messages_address
    ON private_email_messages (tenant_id, address, received_at DESC);
CREATE INDEX IF NOT EXISTS idx_private_email_messages_mailbox
    ON private_email_messages (mailbox_id, received_at DESC);
CREATE INDEX IF NOT EXISTS idx_private_email_messages_unread
    ON private_email_messages (tenant_id) WHERE is_read = false;
CREATE INDEX IF NOT EXISTS idx_private_email_messages_starred
    ON private_email_messages (tenant_id) WHERE is_starred = true;
CREATE INDEX IF NOT EXISTS idx_private_email_messages_ticket
    ON private_email_messages (ticket_id) WHERE ticket_id IS NOT NULL;

-- Labels are their own object (B205): a name and a colour, per account, applied to one or many
-- messages. Deleting a label removes the label only — never a message.
CREATE TABLE IF NOT EXISTS private_email_labels (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name        varchar(64) NOT NULL,
    color       varchar(16) NOT NULL DEFAULT '#64748b',
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, name)
);

CREATE TABLE IF NOT EXISTS private_email_message_labels (
    message_id  uuid NOT NULL REFERENCES private_email_messages(id) ON DELETE CASCADE,
    label_id    uuid NOT NULL REFERENCES private_email_labels(id) ON DELETE CASCADE,
    PRIMARY KEY (message_id, label_id)
);

CREATE INDEX IF NOT EXISTS idx_private_email_message_labels_label
    ON private_email_message_labels (label_id);

-- BACKFILL — the mail that was already lost was not entirely lost.
--
-- Every authenticated inbound delivery DID write an `events` row (source='private_email',
-- event_type='email_received') carrying the sender, recipient, subject and the first 500
-- characters of the body. That is a thin record of real customer mail: enough to SHOW the message
-- in the inbox, flagged as inbox-only, and enough to prove the class of defect (mail accepted,
-- acknowledged and invisible). Only rows with no counterpart in the new table are inserted, so
-- this is safe to run against a database that has already been receiving mail.
INSERT INTO private_email_messages (
    tenant_id, mailbox_id, address, folder, direction,
    from_address, from_name, to_address, subject, body_plain, snippet,
    message_id, is_read, is_ticket, received_at, created_at, updated_at
)
SELECT
    e.tenant_id,
    b.id,
    lower(coalesce(nullif(e.payload->>'to', ''), 'unknown')),
    'inbox',
    'inbound',
    lower(coalesce(e.payload->>'from', '')),
    split_part(lower(coalesce(e.payload->>'from', '')), '@', 1),
    coalesce(e.payload->>'to', ''),
    coalesce(nullif(e.title, '(no subject)'), ''),
    coalesce(e.payload->>'body_preview', ''),
    left(coalesce(e.payload->>'body_preview', ''), 500),
    nullif(e.payload->>'message_id', ''),
    true,
    false,
    e.created_at,
    e.created_at,
    e.created_at
FROM events e
LEFT JOIN private_email_boxes b
       ON b.tenant_id = e.tenant_id
      AND lower(b.email_address) = lower(coalesce(nullif(e.payload->>'to', ''), 'unknown'))
WHERE e.source = 'private_email'
  AND e.event_type = 'email_received'
  AND e.tenant_id IS NOT NULL
  AND NOT EXISTS (
        SELECT 1 FROM private_email_messages m
         WHERE m.tenant_id = e.tenant_id
           AND nullif(e.payload->>'message_id', '') IS NOT NULL
           AND m.message_id = e.payload->>'message_id'
  );
