-- Google Calendar becomes a tenant-level BYOK slot (kanban t_cb0ddb83, CS-4).
--
-- src/google_calendar read GOOGLE_CLIENT_ID / GOOGLE_CLIENT_SECRET straight out of the process
-- environment, which is not something a customer can configure — the exact "env-var-only
-- configuration is a defect" rule. With this catalogue row the tenant BYOK panel offers the slot
-- (GET /api/available-providers) and the module resolves the TENANT's own credentials first,
-- falling back to the environment for centrally-configured deployments.
--
-- Shape: the client SECRET goes in `provider_keys.api_key` (encrypted at rest like every other
-- provider secret) and the client ID in `provider_keys.metadata.client_id`. `requires_metadata`
-- tells the UI to ask for it.
--
-- Idempotent: `available_providers.key` is the primary key, and the upsert keeps the row
-- name/description in sync if it already exists.
INSERT INTO available_providers (key, name, description, requires_base_url, requires_metadata, icon)
VALUES (
    'google_calendar',
    'Google Calendar',
    'Your own Google OAuth client (client ID + secret) for two-way calendar sync',
    false,
    '["client_id"]'::jsonb,
    '📅'
)
ON CONFLICT (key) DO UPDATE SET
    name = EXCLUDED.name,
    description = EXCLUDED.description,
    requires_metadata = EXCLUDED.requires_metadata,
    icon = EXCLUDED.icon;
