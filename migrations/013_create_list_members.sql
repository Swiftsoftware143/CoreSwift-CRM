-- 013_create_list_members.sql
-- Contact membership within lists

CREATE TABLE IF NOT EXISTS list_members (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    list_id UUID NOT NULL REFERENCES lists(id) ON DELETE CASCADE,
    contact_id UUID NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
    added_by UUID REFERENCES users(id) ON DELETE SET NULL,
    added_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(list_id, contact_id)
);

CREATE INDEX IF NOT EXISTS idx_list_members_list ON list_members(list_id);
CREATE INDEX IF NOT EXISTS idx_list_members_contact ON list_members(contact_id);
