-- A failed send must reach a person.
--
-- Measured 2026-11-15: a message to a real address was refused (Mailgun 401 for palmbaypulse.com) and the only
-- trace was `outbound_messages.status='failed'` plus an error string. Nobody was told. It surfaced three days
-- later, by accident, during an audit. A customer who never got their mail is the most expensive kind of silence.
--
-- This is the ledger that makes the alarm fire ONCE per failed message. Without it the sweep would re-alert the
-- same failure every tick until somebody acknowledged it, which is how an alert becomes noise and then becomes
-- ignored. One row per message, written in the same transaction as the notification that names it.

CREATE TABLE IF NOT EXISTS send_failure_alerts (
    message_id  uuid PRIMARY KEY REFERENCES outbound_messages(id) ON DELETE CASCADE,
    tenant_id   uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    alerted_at  timestamptz NOT NULL DEFAULT now(),
    summary     text
);

CREATE INDEX IF NOT EXISTS idx_send_failure_alerts_tenant
    ON send_failure_alerts (tenant_id, alerted_at DESC);
