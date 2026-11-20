-- Email compliance: opt-out, suppression, delivery evidence.
--
-- WHY THIS EXISTS (2026-11-19, David): the app could send mail but could not answer three questions a
-- sending customer must be able to answer — "is my mail being delivered", "who has told me to stop",
-- and "can I prove both". Measured before this migration: `outbound_messages` held only what the app
-- itself attempted (95 sent, 5 failed); no provider event (delivered / bounced / complained) was ever
-- consumed, no unsubscribe link or List-Unsubscribe header existed on ANY send path, and nothing in
-- the codebase ever wrote an opt-out. Bulk mail without an opt-out is unlawful (CAN-SPAM, GDPR) and
-- is the fastest way to have a sending domain blacklisted — the same domain that carries the
-- customer's own replies.
--
-- ONE RULE FOR EVERY CLASS OF MAIL. Platform/system mail, operator (admin) alerts, customer
-- notifications and campaigns all pass the same gate:
--   * transactional  — receipts, password resets, security notices. Exempt from opt-out BY LAW, and
--                      must keep working even for someone who unsubscribed from everything. Still
--                      logged, still event-tracked, still honoured when the address HARD-BOUNCED or
--                      COMPLAINED (sending to a known-bad address hurts every other recipient).
--   * notification   — "your ticket was answered", digests, assignment alerts. Stoppable.
--   * bulk           — campaigns, sequences, marketing. Stoppable, and carries the opt-out footer,
--                      the postal address and the one-click headers.
--
-- Additive only: no existing table is altered or dropped.

CREATE TABLE IF NOT EXISTS email_suppressions (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    -- Always stored lowercased+trimmed: the same human typing DAVID@x.com and david@x.com is one
    -- suppression, and a provider event carries whatever case the remote said.
    address     text NOT NULL,
    -- 'all'  = stop everything except transactional (the "unsubscribe from everything" choice)
    -- 'bulk' / 'notification' = that lane only
    -- 'transactional' is never written by an opt-out; it is written by a hard bounce or complaint,
    -- where delivering at all is the harm.
    mail_class  text NOT NULL DEFAULT 'all' CHECK (mail_class IN ('all', 'bulk', 'notification', 'transactional')),
    reason      text NOT NULL CHECK (reason IN ('unsubscribed', 'hard_bounce', 'complaint', 'manual', 'provider')),
    source      text,
    created_at  timestamptz NOT NULL DEFAULT now(),
    updated_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, address, mail_class)
);

CREATE INDEX IF NOT EXISTS idx_email_suppressions_lookup
    ON email_suppressions (tenant_id, address);

-- One row per send ATTEMPT, written before the provider is asked, so an event the provider reports
-- later can be attributed to this account and this class of mail. `status` records what THIS app did
-- ('sent', 'refused_suppressed', 'failed'); the outcome after the provider took it lives in
-- email_events. provider_message_id is the join key Mailgun returns in its `Message-Id` header.
CREATE TABLE IF NOT EXISTS email_message_log (
    id                    uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id             uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    outbound_message_id   uuid,
    mail_class            text NOT NULL CHECK (mail_class IN ('bulk', 'notification', 'transactional')),
    address               text NOT NULL,
    subject               text,
    provider              text,
    provider_message_id   text,
    status                text NOT NULL CHECK (status IN ('sent', 'failed', 'refused_suppressed')),
    detail                text,
    created_at            timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_email_message_log_tenant
    ON email_message_log (tenant_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_email_message_log_pmid
    ON email_message_log (provider_message_id);

-- What the provider actually did. Rates are COMPUTED from these rows on read — never stored as a
-- number, because a stored rate is a number that drifts from the events it claims to summarise.
CREATE TABLE IF NOT EXISTS email_events (
    id                    uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id             uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    provider              text NOT NULL DEFAULT 'mailgun',
    provider_message_id   text,
    address               text,
    event                 text NOT NULL,
    severity              text,
    reason                text,
    occurred_at           timestamptz NOT NULL,
    raw                   jsonb,
    created_at            timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_email_events_tenant
    ON email_events (tenant_id, occurred_at DESC);
-- Replay protection: the provider retries a webhook it thinks failed, and that must not double-count
-- a bounce into the rate. One row per (message, event, recipient, timestamp) — the provider's own
-- identity for the occurrence.
CREATE UNIQUE INDEX IF NOT EXISTS uq_email_events_occurrence
    ON email_events (provider, coalesce(provider_message_id, ''), event, coalesce(address, ''), occurred_at);

-- Proof of consent: what a person agreed to, when, how. An unsubscribe writes 'revoked' here rather
-- than deleting history — "they opted out on this date from this page" is the record that answers a
-- complaint, and a deleted record answers nothing.
CREATE TABLE IF NOT EXISTS email_consent (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id   uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    address     text NOT NULL,
    mail_class  text NOT NULL DEFAULT 'bulk' CHECK (mail_class IN ('bulk', 'notification')),
    state       text NOT NULL CHECK (state IN ('granted', 'revoked')),
    how         text,
    ip          text,
    user_agent  text,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_email_consent_tenant
    ON email_consent (tenant_id, address, created_at DESC);
