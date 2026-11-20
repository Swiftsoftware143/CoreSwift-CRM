-- kanban t_e2017eb0 — the Telnyx ACCOUNT Ed25519 signing key must be settable where the operator
-- already configures Telnyx, not only in the deploy env.
--
-- WHY. Both public Telnyx receivers (`POST /api/telnyx/webhook`, `POST /api/telnyx/sms-webhook`)
-- fail CLOSED while no account public key is configured: every delivery is answered
-- `503 telnyx_verification_not_configured` and nothing is applied, so inbound calls and inbound SMS
-- — the product's integration itself — cannot work at all. On this deployment that key existed ONLY
-- as `TELNYX_PUBLIC_KEY` in /etc/swift/env/coreswift.env (a root-owned file plus a container
-- restart), while every other Telnyx value (`api_key`, `profile_id`, `messaging_profile_id`) is set
-- from the admin console's Telnyx panel.
--
-- The column is ADDITIVE: `NOT NULL DEFAULT ''` means every existing row reads exactly as before,
-- and a deployment that sets `TELNYX_PUBLIC_KEY` in its environment is byte-identical in behaviour
-- because the ENV still wins (see `telnyx::handlers::resolve_signing_key`).
--
-- Idempotent, like every other file in the sqlx runner (recorded once per version; IF NOT EXISTS
-- keeps a re-run harmless).

ALTER TABLE telnyx_config
    ADD COLUMN IF NOT EXISTS public_key TEXT NOT NULL DEFAULT '';

COMMENT ON COLUMN telnyx_config.public_key IS
    'Telnyx ACCOUNT Ed25519 public key (base64, 32 bytes) used to verify inbound webhook deliveries. Empty = not configured. TELNYX_PUBLIC_KEY in the deploy environment takes precedence when set.';
