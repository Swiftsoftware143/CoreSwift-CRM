-- Migration 021: Tracked Links + Extended Automation Actions

-- Tracked links table
CREATE TABLE IF NOT EXISTS tracked_links (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  tenant_id UUID NOT NULL REFERENCES tenants(id),
  tag_id UUID NOT NULL REFERENCES tags(id),
  slug TEXT NOT NULL UNIQUE,
  target_url TEXT NOT NULL,
  created_at TIMESTAMPTZ DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_tracked_links_slug ON tracked_links(slug);
CREATE INDEX IF NOT EXISTS idx_tracked_links_tenant ON tracked_links(tenant_id);

-- Link clicks table
CREATE TABLE IF NOT EXISTS link_clicks (
  id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
  tracked_link_id UUID NOT NULL REFERENCES tracked_links(id),
  contact_id UUID NOT NULL REFERENCES contacts(id),
  tenant_id UUID NOT NULL REFERENCES tenants(id),
  clicked_at TIMESTAMPTZ DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_link_clicks_link ON link_clicks(tracked_link_id);
CREATE INDEX IF NOT EXISTS idx_link_clicks_contact ON link_clicks(contact_id);

-- Add execution_count and last_executed_at if not already present
-- (They already exist in the schema per the previous inspection)
