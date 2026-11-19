-- Opting out of ONE campaign, not all of them.
--
-- David, 2026-11-19: the unsubscribe page must list the campaigns that address is actually in, with a
-- checkbox beside each, plus "unsubscribe from everything". Before this, an opt-out could only be
-- class-wide: a person who wanted one newsletter to stop had to choose between tolerating it and
-- losing every message the account sends.
--
-- Kept in its own table rather than as a nullable column on `email_suppressions` on purpose:
-- `email_suppressions` rows mean "do not mail this class", and they are what the send-time gate reads.
-- A campaign opt-out means something narrower — "do not mail THIS campaign" — and folding the two
-- together would make every campaign opt-out read as a total one.
--
-- Additive only.

CREATE TABLE IF NOT EXISTS email_campaign_optouts (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    address     text NOT NULL,
    campaign_id uuid NOT NULL REFERENCES email_campaigns(id) ON DELETE CASCADE,
    source      text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, address, campaign_id)
);

CREATE INDEX IF NOT EXISTS idx_email_campaign_optouts_lookup
    ON email_campaign_optouts (tenant_id, address);
