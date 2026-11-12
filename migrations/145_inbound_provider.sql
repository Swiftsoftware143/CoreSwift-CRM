-- Sending and receiving are TWO separate choices (kanban t_04db9228).
--
-- Until now one column (`provider_type`) answered both questions, which forced every customer who
-- wanted their own inbound receiver to also move their OUTBOUND mail. Sending has been
-- provider-agnostic for a while (`mailgun` + `smtp`, and any SMTP service — Amazon SES, SendGrid,
-- Postmark, Google Workspace — sends through the smtp provider), so the missing half was the
-- receiver: `POST /api/v1/webhooks/mailgun/inbound` was the only one that existed.
--
-- `provider_type` keeps meaning SENDING (`mailgun` | `smtp`). This new column means RECEIVING
-- (`mailgun` | `sendgrid` | `none`), so a workspace can send through SES and receive through
-- Mailgun — or send through Mailgun and receive through SendGrid — without either choice
-- overwriting the other.
ALTER TABLE private_email_domains
    ADD COLUMN IF NOT EXISTS inbound_provider varchar(32) NOT NULL DEFAULT 'mailgun';

-- Every row that exists today receives exactly what it already receives with: a Mailgun domain
-- through Mailgun, an SMTP-only domain through nothing (its receiver was never armed — `inbound_mode`
-- was already 'none' on those rows).
UPDATE private_email_domains SET inbound_provider = 'mailgun' WHERE provider_type = 'mailgun';
UPDATE private_email_domains SET inbound_provider = 'none' WHERE provider_type <> 'mailgun';

-- The SendGrid Inbound Parse receiver's credential. SendGrid Inbound Parse signs NOTHING (that is a
-- property of the product, not a choice we made): the only credential it can carry is the URL it is
-- told to post to. So each domain gets its own unguessable token, in the path — the same shape this
-- app already uses for `POST /api/webhook/{token}/{action}`, where the token IS the credential.
--
-- Plaintext on purpose: it must be LOOKED UP by value on the delivery path (an encrypted column
-- cannot be indexed) and DISPLAYED to its owner, who has to paste the URL into their SendGrid
-- account. It is not echoed by the domain list — only by the records/setup response.
ALTER TABLE private_email_domains
    ADD COLUMN IF NOT EXISTS inbound_token varchar(64);

UPDATE private_email_domains
   SET inbound_token = replace(gen_random_uuid()::text, '-', '')
 WHERE inbound_token IS NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_private_email_domains_inbound_token
    ON private_email_domains (inbound_token)
 WHERE inbound_token IS NOT NULL;

-- A domain may only claim a receiving provider that really has a receiver in this binary. Without
-- this, a typo in a request body would store a value that no receiver answers to — a dropdown entry
-- that does nothing, enforced in the database as well as the handler.
ALTER TABLE private_email_domains
    ADD CONSTRAINT private_email_domains_inbound_provider_check
    CHECK (inbound_provider IN ('mailgun', 'sendgrid', 'none'));
