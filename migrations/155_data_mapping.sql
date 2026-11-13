-- 155 — visual data mapping: stored mappings + run history (CoreSwift side)
--
-- Contract: /opt/swift/docs/coreswift-data-mapping-spec-2026-11-13.md §4 and §6.
--
-- Two tables:
--   integration_mappings      the saved mapping (source app/entity, destination, field_map templates,
--                             dedupe rule, options). One row per mapping, tenant-scoped.
--   integration_mapping_runs  every attempt: what matched, what was refused and why, how long it took.
--                             This is what the Runs tab reads, and it is the idempotency guard —
--                             a replayed event collides on (mapping_id, event_id) and becomes a no-op.
--
-- The contact row itself is NEVER written from here: the runner calls the same writer the external
-- contact endpoint uses, so there is exactly one insert path, one dedupe rule and one place where
-- tenancy is enforced.

CREATE TABLE IF NOT EXISTS integration_mappings (
    id           uuid PRIMARY KEY,
    tenant_id    uuid NOT NULL,
    name         text NOT NULL,
    -- {"app":"funnelswift","entity":"leads","schema_version":1}
    source       jsonb NOT NULL,
    -- {"entity":"contact","list_id":null}
    destination  jsonb NOT NULL,
    -- [{"to":"first_name","template":"{{first_name}}","transform":"trim","required":false,"default":null}]
    field_map    jsonb NOT NULL,
    -- {"keys":["email"],"on_match":"update"}   on_match: update | skip | create_new
    dedupe       jsonb NOT NULL,
    -- {"create_missing_fields":false,"skip_empty":true}
    options      jsonb NOT NULL DEFAULT '{}'::jsonb,
    enabled      boolean NOT NULL DEFAULT true,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_integration_mappings_tenant
    ON integration_mappings (tenant_id, enabled);

-- A tenant may not hold two mappings with the same name — the console lists them by name.
CREATE UNIQUE INDEX IF NOT EXISTS idx_integration_mappings_tenant_name
    ON integration_mappings (tenant_id, lower(name));

CREATE TABLE IF NOT EXISTS integration_mapping_runs (
    id            uuid PRIMARY KEY,
    tenant_id     uuid NOT NULL,
    mapping_id    uuid NOT NULL REFERENCES integration_mappings(id) ON DELETE CASCADE,
    -- Caller-supplied idempotency key. NULL for interactive previews, which are never reused.
    event_id      text,
    source_row_id text,
    -- created | updated | skipped | refused | error | replayed
    status        text NOT NULL,
    contact_id    uuid,
    -- [{"field":"email","reason":"required but empty"}]
    refused       jsonb,
    reason        text,
    ms            integer,
    -- the exact payload the resolver produced, so the Runs tab can show what really happened
    payload       jsonb,
    created_at    timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_integration_mapping_runs_mapping
    ON integration_mapping_runs (mapping_id, created_at DESC);

-- Idempotency: the second write of the same event for the same mapping collides here.
CREATE UNIQUE INDEX IF NOT EXISTS idx_integration_mapping_runs_event
    ON integration_mapping_runs (mapping_id, event_id)
    WHERE event_id IS NOT NULL;
