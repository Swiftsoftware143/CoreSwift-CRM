-- 128_private_email_folders.sql
--
-- Real, server-side subfolders (kanban B205, David 2026-11-11: "Create subfolder ... subfolders must
-- be real server-side folders and must be per workspace" — read ACCOUNT-scoped, per B211: mail
-- belongs to the business, not to a workspace).
--
-- WHY A TABLE AT ALL
--   A message already carries its folder as a path ('inbox/Clients'). That is enough to FILE mail,
--   but not enough to show a folder the user created and has not filed anything into yet: an empty
--   folder would exist only in the browser, and would be gone on the next device. The same reason a
--   label is a row (private_email_labels) rather than a string on the message.
--
-- The eight surfaces the sidebar shows (Inbox, Unread, Favorites, Drafts, Sent, Archive, Spam, Trash)
-- are NOT rows here: they are views the server builds from `folder`, `is_read` and `is_starred`.
-- This table holds only what the user makes.

CREATE TABLE IF NOT EXISTS private_email_folders (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    -- The full path, e.g. 'inbox/Clients/Acme'. A path (not an id chain) is what a message stores,
    -- so moving a message and listing a folder agree by construction.
    path        varchar(160) NOT NULL,
    -- The leaf name shown in the rail.
    name        varchar(64) NOT NULL,
    -- The system folder this nests under: inbox | archive | sent | drafts | spam | trash.
    parent      varchar(16) NOT NULL DEFAULT 'inbox',
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, path)
);

CREATE INDEX IF NOT EXISTS idx_private_email_folders_tenant
    ON private_email_folders (tenant_id, parent);
