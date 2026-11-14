-- 160_booking_notifications.sql
--
-- What happens around a booking that is not the booking itself: telling people, and tagging the
-- contact so the booking is visible in the CRM.
--
-- Everything here is CONFIGURATION per calendar, deliberately separate from the booking tables: an
-- owner turns a reminder on or off, names a tag, adds a team address, and nothing about the engine's
-- capacity or pricing depends on it.

-- Who hears about a booking, besides the customer.
create table if not exists booking_notify_emails (
    id          uuid primary key default gen_random_uuid(),
    tenant_id   uuid not null references tenants(id) on delete cascade,
    calendar_id uuid not null references booking_calendars(id) on delete cascade,
    email       text not null,
    label       text not null default '',
    is_active   boolean not null default true,
    created_at  timestamptz not null default now(),
    unique (calendar_id, email)
);
create index if not exists idx_booking_notify_emails_cal on booking_notify_emails (calendar_id)
    where is_active;

-- Tags stamped on the contact when a booking is confirmed or cancelled. `mode` lets a cancel either
-- add a "Cancelled" tag or take the confirmed one back off, which are different questions.
create table if not exists booking_tag_rules (
    id          uuid primary key default gen_random_uuid(),
    tenant_id   uuid not null references tenants(id) on delete cascade,
    calendar_id uuid not null references booking_calendars(id) on delete cascade,
    tag_name    text not null,
    stage       text not null default 'confirm',
    mode        text not null default 'add',
    is_active   boolean not null default true,
    created_at  timestamptz not null default now(),
    constraint booking_tag_rules_stage_chk check (stage in ('confirm', 'cancel')),
    constraint booking_tag_rules_mode_chk check (mode in ('add', 'remove')),
    unique (calendar_id, tag_name, stage)
);
create index if not exists idx_booking_tag_rules_cal on booking_tag_rules (calendar_id)
    where is_active;

-- When to remind, and whom. Offsets are minutes before the booking starts.
create table if not exists booking_reminders (
    id             uuid primary key default gen_random_uuid(),
    tenant_id      uuid not null references tenants(id) on delete cascade,
    calendar_id    uuid not null references booking_calendars(id) on delete cascade,
    offset_minutes integer not null check (offset_minutes > 0),
    audience       text not null default 'customer',
    is_active      boolean not null default true,
    created_at     timestamptz not null default now(),
    constraint booking_reminders_audience_chk check (audience in ('customer', 'team')),
    unique (calendar_id, offset_minutes, audience)
);

-- Proof a reminder went out, so a restart cannot send the same one twice.
create table if not exists booking_reminder_sends (
    id             uuid primary key default gen_random_uuid(),
    booking_id     uuid not null references booking_bookings(id) on delete cascade,
    offset_minutes integer not null,
    audience       text not null,
    sent_at        timestamptz not null default now(),
    unique (booking_id, offset_minutes, audience)
);

-- Per-calendar mail behaviour: where a reply should land, and whether a confirmation is sent at all.
alter table booking_calendars add column if not exists reply_to_email text;
alter table booking_calendars add column if not exists send_confirmation boolean not null default true;

-- Global default templates. A workspace may override any of them by inserting its own row with the
-- same template_type (tenant-scoped wins), which is how the rest of the app already works.
insert into email_templates (template_type, name, subject, body, html_body, is_default, tenant_id)
select v.template_type, v.name, v.subject, v.body, v.html_body, true, null
  from (values
    ('booking_confirmed',
     'Booking confirmed',
     'Your booking with {{calendar_name}} is confirmed',
     E'Hello {{customer_name}},\n\nYour booking with {{calendar_name}} is confirmed.\n\n{{items}}\n\nTotal: {{total}}\n\nNeed to change it? Open this link — you can move it, cancel it, or book it again:\n{{manage_url}}\n\nJust reply to this email if you would rather speak to a person.\n',
     null),
    ('booking_team_heads_up',
     'New booking (team)',
     'New booking: {{customer_name}} — {{first_item}}',
     E'{{customer_name}} ({{customer_email}}) has booked with {{calendar_name}}.\n\n{{items}}\n\nTotal: {{total}}\n\nContact: {{customer_email}}\n',
     null),
    ('booking_reminder',
     'Booking reminder',
     'A reminder: {{first_item}} with {{calendar_name}}',
     E'Hello {{customer_name}},\n\nA reminder about your booking with {{calendar_name}}.\n\n{{items}}\n\nNeed to change it?\n{{manage_url}}\n',
     null)
  ) as v(template_type, name, subject, body, html_body)
 where not exists (
     select 1 from email_templates t where t.template_type = v.template_type and t.tenant_id is null
 );
