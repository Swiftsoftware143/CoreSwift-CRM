-- Portal sign-in by emailed one-time link.
--
-- Decision on record (docs/ticketing-suite-plan.md, decision 1): the customer panel gets an emailed
-- one-time link AND keeps the address+reference capability proof underneath as the fallback, because a
-- login that depends on a credential the tenant may not have bought turns a support page into a support
-- ticket.
--
-- WHY A TABLE AND NOT JUST A SIGNED TOKEN: "one time" needs state. A signed token can only say "this was
-- issued for this address"; it cannot say "and it has already been used", and a link that keeps working
-- after it is used is a password in an inbox forever.
--
-- The token is stored as a SHA-256 HASH, never in clear: the row is a record that a link was issued, not
-- a copy of the credential. A leaked database therefore yields no working links.

CREATE TABLE IF NOT EXISTS portal_login_tokens (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    address     text NOT NULL,
    token_hash  text NOT NULL,
    expires_at  timestamptz NOT NULL,
    used_at     timestamptz,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_portal_login_tokens_hash ON portal_login_tokens (token_hash);
CREATE INDEX IF NOT EXISTS idx_portal_login_tokens_address
    ON portal_login_tokens (tenant_id, address, created_at DESC);

-- The message the link arrives in. Seeded as a DEFAULT so every workspace has it without anyone
-- configuring a template first; a workspace that wants its own wording overrides it like any other.
INSERT INTO email_templates (id, template_type, name, subject, body, is_default, created_at, updated_at)
SELECT gen_random_uuid(), 'support_portal_link', 'Support portal sign-in link',
       'Your sign-in link for support',
       E'Hello,\n\nUse this link to see the requests you have sent us. It works once and expires in 30 minutes:\n\n{{link}}\n\nIf you did not ask for this, you can ignore this message.\n',
       true, now(), now()
WHERE NOT EXISTS (
    SELECT 1 FROM email_templates WHERE template_type = 'support_portal_link'
);
