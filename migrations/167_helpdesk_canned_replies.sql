-- Saved answers, because the same question is asked fifty times.
--
-- A canned reply is a body of text with placeholders the server-side renderer fills at the moment it is
-- inserted into a reply — so the text an agent sends is the text that lands, not a template with braces in
-- it. The substitutions are deliberately few and local (`{{contact.first_name}}`, `{{ticket.number}}`,
-- `{{agent.first_name}}`, `{{site.name}}`): anything richer becomes a template language nobody can debug at
-- 6pm on a Friday.
--
-- A `site_id`-scoped reply is the spec's own model; this maps a site onto the ACCOUNT (see
-- docs/helpdesk-mapping.md), so `tenant_id` is the scope and a null owner means "everyone in the account".

CREATE TABLE IF NOT EXISTS helpdesk_canned_replies (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id  uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    title      text NOT NULL,
    -- what an agent types after a slash: /refund
    shortcut   text,
    body_md    text NOT NULL,
    owner_id   uuid REFERENCES users(id) ON DELETE SET NULL,
    shared     boolean NOT NULL DEFAULT true,
    use_count  integer NOT NULL DEFAULT 0,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_helpdesk_canned_tenant
    ON helpdesk_canned_replies (tenant_id, title);

-- One shortcut per account: if two replies answered to /refund, an agent typing it would have no way to
-- predict which one went out.
CREATE UNIQUE INDEX IF NOT EXISTS idx_helpdesk_canned_shortcut
    ON helpdesk_canned_replies (tenant_id, lower(shortcut))
    WHERE shortcut IS NOT NULL AND shortcut <> '';
