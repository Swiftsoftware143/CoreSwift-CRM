-- 130_incentiveswift_connector_catalogue.sql
--
-- IncentiveSwift is a first-party capture/loyalty app CoreSwift can now connect
-- (src/native_apps/connectors/incentiveswift.rs). The Integration Center renders its native
-- connectors from THIS table -- `GET /api/integration-center/overview` selects
-- `FROM available_providers ... WHERE ap.key = ANY(<NATIVE_APPS slugs>)` -- so a connector with
-- no catalogue row is invisible on that surface even though `/api/native/apps` lists it.
-- Measured 2026-11-11 before this row: the catalogue held adaswift, funnelswift, workflowswift,
-- missedcall-responder and multi-directory only, and no IncentiveSwift connection was possible
-- (POST /api/native/apps/incentiveswift/test answered 404 "App 'incentiveswift' not found").
--
-- Additive + idempotent: `key` is the primary key, the upsert keeps the row in step with the
-- connector's own name/description, and re-running changes nothing. `coming_soon` keeps its
-- default (false) -- this connector is connectable today, unlike the retired/BYOK placeholders
-- migration 119 flags.
--
-- No key is minted here and no connection is created: the pilot connection is a separate card.

INSERT INTO available_providers (key, name, description, requires_base_url, requires_metadata, icon)
VALUES (
    'incentiveswift',
    'IncentiveSwift',
    'Loyalty and rewards engine — programmes, campaigns, members and points. Pull campaigns, contacts and programmes into CoreSwift, or push CRM contacts into a loyalty programme.',
    true,
    '[]'::jsonb,
    'gift'
)
ON CONFLICT (key) DO UPDATE
    SET name              = EXCLUDED.name,
        description       = EXCLUDED.description,
        requires_base_url = EXCLUDED.requires_base_url,
        icon              = EXCLUDED.icon;
