-- 016_create_tag_mappings.sql
-- Cross-system tag mappings (local → external system)

CREATE TABLE IF NOT EXISTS tag_mappings (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    integration_id UUID NOT NULL REFERENCES integrations(id) ON DELETE CASCADE,
    tag_id UUID NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
    external_system VARCHAR(100) NOT NULL,
    external_id VARCHAR(255) NOT NULL,
    external_name VARCHAR(255),
    direction VARCHAR(20) NOT NULL DEFAULT 'bidirectional' 
        CHECK (direction IN ('outbound', 'inbound', 'bidirectional')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_tag_mappings_integration ON tag_mappings(integration_id);
CREATE INDEX IF NOT EXISTS idx_tag_mappings_tag ON tag_mappings(tag_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_tag_mappings_unique ON tag_mappings(integration_id, tag_id, external_system);
