-- A record of what was asked of a model, and what came back.
CREATE TABLE IF NOT EXISTS helpdesk_kb_generations (
    id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id  uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    asked_by   uuid REFERENCES users(id) ON DELETE SET NULL,
    about      text NOT NULL,
    industry   text,
    audience   text,
    count      integer NOT NULL DEFAULT 0,
    drafted    integer NOT NULL DEFAULT 0,
    provider   text NOT NULL DEFAULT 'auto',
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_helpdesk_kb_generations_recent
    ON helpdesk_kb_generations (tenant_id, created_at DESC);
