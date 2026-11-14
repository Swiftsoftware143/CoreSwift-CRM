-- 161_booking_host_settings.sql
--
-- What a BOOKING ADDRESS does when someone opens it with no calendar in the path.
--
-- David, 2026-11-14: "users should be able to redirect that custom domain. For example if a user goes
-- to http://booking.swiftsoftware.net I should be able to redirect them wherever I want. Including
-- another calendar."
--
-- So the root of a booking host is CONFIGURATION, per workspace: send them to a URL of the owner's
-- choosing (another calendar, their main site, a campaign page), or let CoreSwift show the workspace's
-- own list of bookable calendars. Never a dead page.
create table if not exists booking_host_settings (
    tenant_id              uuid primary key references tenants(id) on delete cascade,
    root_redirect_url      text,
    default_calendar_slug  text,
    updated_at             timestamptz not null default now()
);

comment on table booking_host_settings is
    'What a booking address does at its root: redirect somewhere the owner chose, or list this workspace''s bookable calendars. Empty means list them.';
