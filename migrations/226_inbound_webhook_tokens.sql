-- Replay defence for signed inbound webhooks.
--
-- Mailgun signs ONLY (timestamp + token). Measured live 2026-11-27: the same timestamp/token/signature
-- replayed to a DIFFERENT recipient answered 200 and stored a second message. Nothing in the signature
-- binds it to a recipient or a body, so the only defence available on this scheme is:
--   * a FRESHNESS window enforced on the server clock, and
--   * a SINGLE-USE claim on the token, in a durable store, taken BEFORE any side effect.
--
-- Durable, not in-memory: the route can be served by more than one process, and a replay that lands on
-- the other process must still be refused. The UNIQUE primary key is the claim; the insert IS the lock.
CREATE TABLE IF NOT EXISTS inbound_webhook_tokens (
    token       text        PRIMARY KEY,
    provider    text        NOT NULL DEFAULT 'mailgun',
    received_at timestamptz NOT NULL DEFAULT now()
);

-- Cleanup walks received_at. The ledger must outlive the freshness window (2x), so the row that stops a
-- replay is still there while the replay is still fresh enough to be attempted.
CREATE INDEX IF NOT EXISTS inbound_webhook_tokens_received_at_idx
    ON inbound_webhook_tokens (received_at);
