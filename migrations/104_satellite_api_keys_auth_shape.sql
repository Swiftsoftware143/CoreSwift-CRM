-- 104_satellite_api_keys_auth_shape.sql
--
-- WHAT ACTUALLY AUTHENTICATES AN INBOUND SATELLITE CALL (kanban t_588a15d1)
--
-- Before this migration the shape was ambiguous in the store: `key_hash` was NOT NULL and
-- written by 030_inbound_webhook.sql, but no code read it — the receiver authenticated on
-- `key_prefix` alone (a short string the served guide documents as living in the webhook URL).
-- A column named key_hash that nobody verifies reads like a verified secret to the next
-- engineer, so the store now says what the receiver in src/inbound/handlers.rs enforces:
--
--   * the URL carries ONLY the prefix  ->  an index, never a credential
--   * the credential is the full key in the `X-Satellite-Key` request header
--   * its SHA-256 hex digest (lowercase, 64 chars) must equal `key_hash`, compared in
--     constant time; any failure (missing header, unknown prefix, inactive row, mismatch,
--     or a row whose key_hash is not a 64-char hex digest) answers 401
--
-- Format: SHA-256 hex, exactly the digest `sha256_hex()` computes in
-- src/inbound/handlers.rs (same construction as the personal API-key hasher in
-- src/personal_api_keys.rs). Keying a row (operator provisioning; there is no console screen):
--
--   INSERT INTO satellite_api_keys (tenant_id, name, source_app, key_prefix, key_hash)
--   VALUES ('<tenant uuid>', 'funnelswift-prod', 'funnelswift',
--           substr('<full-key>', 1, 8),
--           encode(sha256('<full-key>'::bytea), 'hex'));
--
-- Rotate by UPDATE ... SET key_hash = encode(sha256('<new-key>'::bytea),'hex'),
-- key_prefix = substr('<new-key>',1,8) — the full key is never recoverable from the row.
-- Live rows at the time of this migration: 0 (nothing to back-fill; a legacy row holding a
-- plaintext key in key_hash simply fails the length check and authenticates nothing).

COMMENT ON COLUMN public.satellite_api_keys.key_hash IS
    'SHA-256 hex digest (lowercase, 64 chars) of the FULL satellite key. The caller presents the full key in the X-Satellite-Key header; the receiver compares its digest with this column in constant time (src/inbound/handlers.rs). The key itself is never stored. A value that is not a 64-char hex digest authenticates nothing.';

COMMENT ON COLUMN public.satellite_api_keys.key_prefix IS
    'Public, non-secret INDEX: the first characters of the key, used in the webhook URL path (/inbound/<key_prefix>/<event_type>) to select the candidate row. It is NOT the credential — the credential is the full key in X-Satellite-Key.';
