CREATE TABLE IF NOT EXISTS round_robin_teams (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    description TEXT,
    strategy VARCHAR(50) NOT NULL DEFAULT 'round_robin',  -- 'round_robin', 'least_loaded', 'weighted'
    scope_type VARCHAR(50) NOT NULL DEFAULT 'global',      -- 'global', 'calendar', 'city'
    scope_id UUID,                                          -- calendar_id if scope_type='calendar'
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_rr_teams_tenant ON round_robin_teams(tenant_id);

CREATE TABLE IF NOT EXISTS round_robin_members (
    id UUID PRIMARY KEY,
    team_id UUID NOT NULL REFERENCES round_robin_teams(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    weight INT NOT NULL DEFAULT 1,          -- for weighted strategy
    max_concurrent_bookings INT DEFAULT 10,  -- for least_loaded
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(team_id, user_id)
);
CREATE INDEX IF NOT EXISTS idx_rr_members_team ON round_robin_members(team_id);

CREATE TABLE IF NOT EXISTS round_robin_assignments (
    id UUID PRIMARY KEY,
    team_id UUID NOT NULL REFERENCES round_robin_teams(id) ON DELETE CASCADE,
    member_id UUID NOT NULL REFERENCES round_robin_members(id) ON DELETE CASCADE,
    booking_id UUID REFERENCES slot_bookings(id) ON DELETE SET NULL,
    contact_id UUID,
    assigned_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    status VARCHAR(50) NOT NULL DEFAULT 'pending'  -- 'pending', 'accepted', 'rejected', 'completed'
);
CREATE INDEX IF NOT EXISTS idx_rr_assignments_team ON round_robin_assignments(team_id);
CREATE INDEX IF NOT EXISTS idx_rr_assignments_member ON round_robin_assignments(member_id);
