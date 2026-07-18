-- Booking Calendar System
-- Enterprise upsell module for multi-calendar/directory slot management

CREATE TABLE IF NOT EXISTS booking_calendars (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    slug TEXT NOT NULL,
    description TEXT,
    calendar_type TEXT NOT NULL DEFAULT 'generic',  -- city, product, generic
    metadata JSONB DEFAULT '{}',
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(tenant_id, slug)
);

CREATE TABLE IF NOT EXISTS calendar_slots (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    calendar_id UUID NOT NULL REFERENCES booking_calendars(id) ON DELETE CASCADE,
    slot_name TEXT NOT NULL,
    total_slots INTEGER NOT NULL DEFAULT 10,      -- -1 = unlimited
    filled_slots INTEGER NOT NULL DEFAULT 0,
    default_duration_days INTEGER NOT NULL DEFAULT 30,
    price_override NUMERIC(10,2),
    coreswift_tag_template TEXT,                    -- e.g. "{city}-banner-ad"
    coreswift_list_id UUID,                        -- optional CoreSwift list to auto-add
    is_active BOOLEAN NOT NULL DEFAULT true,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS slot_bookings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    calendar_id UUID NOT NULL REFERENCES booking_calendars(id) ON DELETE CASCADE,
    slot_id UUID NOT NULL REFERENCES calendar_slots(id) ON DELETE CASCADE,
    contact_id UUID,                                -- CoreSwift contact if matched
    business_name TEXT NOT NULL,
    contact_name TEXT,
    contact_email TEXT NOT NULL,
    contact_phone TEXT,
    website TEXT,
    description TEXT,
    target_audience TEXT,
    call_booking TEXT,                              -- "Yes, call me back" / "Email me instead" / "No"
    start_date DATE NOT NULL,
    end_date DATE NOT NULL,
    slot_position INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending_payment', -- pending_payment, active, cancelled, expired
    price_paid NUMERIC(10,2),
    currency TEXT NOT NULL DEFAULT 'USD',
    stripe_payment_intent_id TEXT,
    stripe_subscription_id TEXT,
    metadata JSONB DEFAULT '{}',                    -- Q&A answers, uploaded files, etc.
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Indexes
CREATE INDEX IF NOT EXISTS idx_booking_calendars_tenant ON booking_calendars(tenant_id);
CREATE INDEX IF NOT EXISTS idx_booking_calendars_slug ON booking_calendars(tenant_id, slug);
CREATE INDEX IF NOT EXISTS idx_calendar_slots_calendar ON calendar_slots(calendar_id);
CREATE INDEX IF NOT EXISTS idx_slot_bookings_tenant ON slot_bookings(tenant_id);
CREATE INDEX IF NOT EXISTS idx_slot_bookings_status ON slot_bookings(tenant_id, status);
CREATE INDEX IF NOT EXISTS idx_slot_bookings_dates ON slot_bookings(start_date, end_date);
CREATE INDEX IF NOT EXISTS idx_slot_bookings_active ON slot_bookings(calendar_id, slot_id, status) WHERE status = 'active';
