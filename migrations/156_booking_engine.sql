-- 156_booking_engine.sql
--
-- ONE model, every calendar type a preset of it. Written from the `coreswift-booking` engine spec
-- (migrations/0001_booking.sql in that module), adapted to this app's existing schema instead of
-- creating a second calendar system beside it.
--
--   booking_calendars            the thing being booked (ALREADY EXISTS here - extended, not replaced)
--   booking_resources            what within it is bookable (a staff member, an ad placement)
--   booking_availability_rules   weekly open hours (appointment calendars)
--   booking_slots                explicit, individually priced stock (inventory calendars)
--   booking_bundles              pricing rules; the single best one applies to an order
--   booking_orders               several bookings paid together
--   booking_bookings             who took which slot, and where it stands
--
-- WHY THIS SHAPE AND NOT THE SPEC'S: the spec's migration opens with CREATE TABLE booking_calendars,
-- and this database already HAS booking_calendars holding 14 live calendars with a different set of
-- columns. Copying it in would fail. So that table gains the columns the engine needs, and the six
-- tables that do not exist here are created.
--
-- Money is integer cents. Times are UTC. Additive only.

-- ---------------------------------------------------------------- the calendar (extend, never replace)

alter table booking_calendars
    add column if not exists kind         text,
    add column if not exists timezone     text        not null default 'America/New_York',
    add column if not exists currency     text        not null default 'usd',
    add column if not exists hold_minutes integer     not null default 30,
    add column if not exists settings     jsonb       not null default '{}'::jsonb;

-- Backfill `kind` for the calendars that already exist. The 13 city calendars hold capacity slots
-- over date spans, which is the inventory shape; anything else starts as an appointment calendar.
update booking_calendars set kind = 'inventory'   where kind is null and calendar_type = 'city';
update booking_calendars set kind = 'appointment' where kind is null;

alter table booking_calendars alter column kind set not null;

do $$
begin
    if not exists (select 1 from pg_constraint where conname = 'booking_calendars_kind_chk') then
        alter table booking_calendars
            add constraint booking_calendars_kind_chk
            check (kind in ('appointment', 'inventory', 'date_range'));
    end if;
    if not exists (select 1 from pg_constraint where conname = 'booking_calendars_hold_minutes_chk') then
        alter table booking_calendars
            add constraint booking_calendars_hold_minutes_chk
            check (hold_minutes between 1 and 240);
    end if;
end $$;

-- The public booking page addresses a calendar by slug alone (/public/booking/c/{slug}), so the slug
-- must be unique platform-wide; here it was only unique per tenant. All 14 existing slugs are already
-- distinct, so this applies cleanly, and a future collision is refused rather than silently letting a
-- new calendar shadow another workspace's public page.
create unique index if not exists idx_booking_calendars_slug_global
    on booking_calendars (slug);

-- ---------------------------------------------------------------- resources

create table if not exists booking_resources (
    id                uuid primary key default gen_random_uuid(),
    calendar_id       uuid not null references booking_calendars(id) on delete cascade,
    name              text not null,
    description       text not null default '',
    capacity          integer not null default 1 check (capacity > 0),
    price_cents       bigint  not null default 0 check (price_cents >= 0),
    duration_min      integer not null default 30 check (duration_min > 0),
    slot_interval_min integer not null default 30 check (slot_interval_min > 0),
    buffer_min        integer not null default 0,
    min_notice_min    integer not null default 0,
    sort_order        integer not null default 0,
    metadata          jsonb not null default '{}'::jsonb,
    active            boolean not null default true,
    created_at        timestamptz not null default now()
);

create index if not exists booking_resources_calendar_idx on booking_resources (calendar_id);

-- ---------------------------------------------------------------- weekly open hours (appointment)

create table if not exists booking_availability_rules (
    id          uuid primary key default gen_random_uuid(),
    resource_id uuid not null references booking_resources(id) on delete cascade,
    weekday     smallint not null check (weekday between 0 and 6),
    start_time  time not null,
    end_time    time not null,
    constraint booking_availability_span_chk check (end_time > start_time)
);

create index if not exists booking_rules_resource_idx on booking_availability_rules (resource_id);

-- ---------------------------------------------------------------- explicit priced stock (inventory)

create table if not exists booking_slots (
    id          uuid primary key default gen_random_uuid(),
    resource_id uuid not null references booking_resources(id) on delete cascade,
    starts_at   timestamptz not null,
    ends_at     timestamptz not null,
    capacity    integer not null default 1 check (capacity > 0),
    price_cents bigint not null check (price_cents >= 0),
    label       text not null default '',
    status      text not null default 'open' check (status in ('open', 'closed')),
    constraint booking_slots_span_chk check (ends_at > starts_at),
    -- Generating the same issue twice cannot duplicate the placement it would sell twice.
    constraint booking_slots_unique_position unique (resource_id, starts_at)
);

create index if not exists booking_slots_time_idx on booking_slots (resource_id, starts_at);

-- ---------------------------------------------------------------- bundle pricing

create table if not exists booking_bundles (
    id                    uuid primary key default gen_random_uuid(),
    calendar_id           uuid not null references booking_calendars(id) on delete cascade,
    name                  text not null,
    kind                  text not null check (kind in ('percent_off', 'fixed_price')),
    min_items             integer not null check (min_items > 0),
    percent_off           integer not null default 0 check (percent_off between 0 and 100),
    fixed_price_cents     bigint  not null default 0 check (fixed_price_cents >= 0),
    eligible_resource_ids uuid[] not null default '{}',
    required_resource_ids uuid[] not null default '{}',
    active                boolean not null default true,
    created_at            timestamptz not null default now(),
    constraint booking_bundles_shape_chk check (
        (kind = 'percent_off' and percent_off > 0)
        or (kind = 'fixed_price' and fixed_price_cents >= 0)
    )
);

create index if not exists booking_bundles_calendar_idx on booking_bundles (calendar_id);

-- ---------------------------------------------------------------- orders (several bookings, one payment)

create table if not exists booking_orders (
    id              uuid primary key default gen_random_uuid(),
    tenant_id       uuid not null references tenants(id) on delete cascade,
    calendar_id     uuid not null references booking_calendars(id),
    contact_id      uuid references contacts(id) on delete set null,
    customer_name   text not null,
    customer_email  text not null,
    status          text not null default 'pending' check (status in ('pending', 'confirmed', 'cancelled')),
    subtotal_cents  bigint not null,
    discount_cents  bigint not null default 0,
    total_cents     bigint not null,
    bundle_id       uuid references booking_bundles(id) on delete set null,
    hold_expires_at timestamptz not null,
    payment_ref     text,
    notes           text not null default '',
    created_at      timestamptz not null default now()
);

create index if not exists booking_orders_tenant_idx on booking_orders (tenant_id, created_at desc);
create index if not exists booking_orders_email_idx  on booking_orders (tenant_id, customer_email);

-- ---------------------------------------------------------------- bookings (a held or confirmed position)

create table if not exists booking_bookings (
    id              uuid primary key default gen_random_uuid(),
    order_id        uuid not null references booking_orders(id) on delete cascade,
    resource_id     uuid not null references booking_resources(id),
    slot_id         uuid references booking_slots(id) on delete set null,
    starts_at       timestamptz not null,
    ends_at         timestamptz not null,
    quantity        integer not null default 1 check (quantity > 0),
    price_cents     bigint not null,
    status          text not null default 'held' check (status in ('held', 'confirmed', 'cancelled')),
    hold_expires_at timestamptz not null,
    meta            jsonb not null default '{}'::jsonb,
    created_at      timestamptz not null default now()
);

create index if not exists booking_bookings_resource_time_idx on booking_bookings (resource_id, starts_at, ends_at);
create index if not exists booking_bookings_slot_idx          on booking_bookings (slot_id);
create index if not exists booking_bookings_order_idx         on booking_bookings (order_id);

comment on column booking_bookings.meta is
    'Per-booking JSON: ad creative, approval state, intake answers. creative_status is null | pending | approved | rejected.';
comment on table booking_bundles is
    'Pricing rules on an order. The single best applicable rule is applied, never a stack of them.';
comment on table booking_orders is
    'Several bookings paid together. A bundle spans one calendar in this milestone.';
