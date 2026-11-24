-- Custom Objects, Phase 1 — the OWNED no-code backend (MCP-first).
--
-- Why: Bolt.new and NoCodeBackend sold exactly one thing we did not have — "describe an app, get a
-- database, tables, fields and a key" — and nothing else in their pitch is missing here (hosting,
-- Postgres, tenancy, key minting, MCP all already run). So the gap is PROVISIONING, not capability:
-- an MCP-capable editor describes a data shape and gets it, with every byte living in this database
-- and every read scoped to one account. Neither vendor has a workload, so this replaces them.
--
-- SCOPE — read this before adding a column. The spec says `workspace_id`; this app's equivalent is
-- `tenant_id`. A personal API key (`personal_api_keys.tenant_id`) resolves to exactly ONE account,
-- and a machine caller has no `workspaces.id` at all, so the ACCOUNT is the only unit both
-- credential classes can agree on. `workspaces` rows are a sub-scope of the same account and are
-- deliberately NOT used here: a custom-object backend belongs to the account you authenticate as.
-- A tenant id is NEVER accepted from a request body — it comes from the credential.
--
-- Nothing in Phase 1 is destructive and nothing emits DDL from tenant input: fields are ARCHIVED
-- (their data stays in `custom_records.data`), records are SOFT-deleted, and there are no per-tenant
-- schemas, views, triggers or migrations beyond add/rename/archive.

CREATE TABLE IF NOT EXISTS custom_objects (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id  UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    slug       TEXT NOT NULL,
    name       TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- The slug IS the URL segment, so its alphabet is fixed here as well as in Rust: a 400 from the
    -- handler is friendlier, but the CHECK is what makes a hand-written INSERT unable to smuggle a
    -- path or a quote into a route.
    CONSTRAINT custom_objects_slug_shape CHECK (slug ~ '^[a-z][a-z0-9_]{0,62}$'),
    CONSTRAINT custom_objects_tenant_slug UNIQUE (tenant_id, slug)
);

CREATE TABLE IF NOT EXISTS custom_fields (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    object_id    UUID NOT NULL REFERENCES custom_objects(id) ON DELETE CASCADE,
    key          TEXT NOT NULL,
    label        TEXT NOT NULL,
    kind         TEXT NOT NULL,
    required     BOOLEAN NOT NULL DEFAULT FALSE,
    unique_value BOOLEAN NOT NULL DEFAULT FALSE,
    options      JSONB NOT NULL DEFAULT '[]'::jsonb,
    position     INTEGER NOT NULL DEFAULT 0,
    archived_at  TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT custom_fields_kind CHECK (kind IN ('text', 'number', 'bool', 'date', 'enum', 'json')),
    CONSTRAINT custom_fields_key_shape CHECK (key ~ '^[a-z][a-z0-9_]{0,62}$'),
    -- Archiving keeps the row, so a re-added key REVIVES it rather than colliding here; the handler
    -- is the one that knows the difference between "already active" (409) and "archived" (restore).
    CONSTRAINT custom_fields_object_key UNIQUE (object_id, key)
);

CREATE INDEX IF NOT EXISTS ix_custom_fields_object
    ON custom_fields (object_id, position);

CREATE TABLE IF NOT EXISTS custom_records (
    id         UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id  UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    object_id  UUID NOT NULL REFERENCES custom_objects(id) ON DELETE CASCADE,
    data       JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    deleted_at TIMESTAMPTZ
);

-- The list query is `WHERE object_id = … ORDER BY created_at DESC`, so the index carries both.
CREATE INDEX IF NOT EXISTS ix_custom_records_object_created
    ON custom_records (object_id, created_at DESC);

-- Filters are jsonb CONTAINMENT (`data @> '{"status":"open"}'`), which jsonb_path_ops serves and the
-- default opclass does not.
CREATE INDEX IF NOT EXISTS ix_custom_records_data
    ON custom_records USING GIN (data jsonb_path_ops);

CREATE TABLE IF NOT EXISTS custom_object_audit (
    id          BIGSERIAL PRIMARY KEY,
    tenant_id   UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    -- WHO: `key:<personal_api_keys.id>` or `user:<users.id>`. Never the key itself, never a password.
    actor       TEXT NOT NULL,
    action      TEXT NOT NULL,
    object_slug TEXT,
    detail      JSONB NOT NULL DEFAULT '{}'::jsonb,
    at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS ix_custom_object_audit_tenant
    ON custom_object_audit (tenant_id, at DESC);
