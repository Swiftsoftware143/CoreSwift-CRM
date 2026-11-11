-- 126 — personal API keys: auto-provisioned, RETRIEVABLE, and auditable.
--
-- David, 2026-11-11 (kanban t_7863696b): the "shown once, never again" model is WITHDRAWN.
-- A key is stored twice from now on:
--   * `key_hash`      — SHA-256 hex, the ONLY thing authentication reads;
--   * `key_sealed`    — the same key sealed with `secret_box` (`enc:v1:…`, AES-256-GCM keyed by
--                       the server secret in the environment AND the tenant id), used ONLY to
--                       display the key back to its owner. Never decrypted on an auth path.
--
-- Rows written before this migration have no `key_sealed` (their plaintext is unrecoverable by
-- design) and are marked for hand-off: the UI offers Regenerate, which mints a fresh key that is
-- both authenticating and revealable.

ALTER TABLE personal_api_keys ADD COLUMN IF NOT EXISTS key_sealed text;

-- The sealed column can only ever hold the sealed envelope. A future writer that forgets to seal
-- fails loudly here instead of quietly storing a live credential in the clear.
ALTER TABLE personal_api_keys DROP CONSTRAINT IF EXISTS personal_api_keys_key_sealed_envelope;
ALTER TABLE personal_api_keys ADD CONSTRAINT personal_api_keys_key_sealed_envelope
    CHECK (key_sealed IS NULL OR key_sealed LIKE 'enc:v1:%');

-- Every reveal is logged: who, when, which key (prefix only — never the secret).
CREATE TABLE IF NOT EXISTS personal_api_key_reveals (
    id          uuid PRIMARY KEY,
    key_id      uuid NOT NULL,
    tenant_id   uuid NOT NULL,
    user_id     uuid,
    key_prefix  text,
    ip_address  text,
    user_agent  text,
    revealed_at timestamp with time zone NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_personal_api_key_reveals_key
    ON personal_api_key_reveals (key_id, revealed_at DESC);

-- One-time fleet backfills. A backfill that already ran must never run a second time: a customer
-- who deliberately revoked every key must not have one silently re-minted on the next boot.
CREATE TABLE IF NOT EXISTS fleet_data_backfills (
    name       text PRIMARY KEY,
    applied_at timestamp with time zone NOT NULL DEFAULT NOW(),
    detail     text
);
