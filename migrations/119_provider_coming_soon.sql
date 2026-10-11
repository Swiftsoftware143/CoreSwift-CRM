-- 119_provider_coming_soon.sql
-- kanban t_046e87b2 — backend half of t_289bc8b0: six catalogue rows this app offers
-- but no code path in it can spend.
--
-- The consoles already render these six as a non-interactive Coming Soon card; until now
-- they did it from a hardcoded bridge list (CS_COMING_SOON_BRIDGE) because the catalogue
-- row carried no flag. This adds the authoritative flag and the API becomes the source of
-- truth for both consoles.
--
-- Why these six, from a reader census of the provider_keys TABLE in src/ (t_289bc8b0):
-- the only readers are src/google_calendar/mod.rs, src/telnyx/handlers.rs,
-- src/provider_keys/handlers.rs itself and src/secret_box.rs (at-rest encryption sweep).
-- src/ai/handlers.rs resolves AI keys from tenants.settings->'ai'->'providers';
-- src/private_email/* uses per-domain credentials (private_email_domains.mailgun_api_key,
-- private_email_api_keys, provider_api_keys); the sister-connector rows are consumed by
-- src/native_apps/connectors/* + src/integration_center.rs through app_connections, NOT
-- through a stored key. A repo-wide case-insensitive grep over src/ for letterman,
-- nexweave, sam_gov, usaspending, sendiio and sendgrid returns ZERO hits, and nothing
-- enumerates providers by available_providers.category, so no indirect resolver reaches
-- them either.
--
-- NOT flagged, with evidence: mailgun (private-email domain credential, real sends),
-- openai/deepseek (AI chain exists), google_calendar + telnyx (real provider_keys
-- readers), adaswift/funnelswift/incentiveswift/workflowswift/missedcall-responder/
-- multi-directory (sister-product connectors, real app_connections arm).
--
-- Additive and idempotent: the column defaults to false, so every row that is not named
-- below keeps the exact behaviour it had (it can still be connected), and re-running the
-- UPDATE on a database that already carries the flag is a no-op.

ALTER TABLE available_providers
    ADD COLUMN IF NOT EXISTS coming_soon boolean NOT NULL DEFAULT false;

UPDATE available_providers
   SET coming_soon = true
 WHERE key IN ('letterman', 'nexweave', 'sam_gov', 'sendgrid', 'sendiio', 'usaspending');
