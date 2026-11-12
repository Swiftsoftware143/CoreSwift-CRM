-- 149 — a calendar can sit on its own subdomain (kanban t_d4ac5433).
--
-- The calendar URL shape is <account-subdomain>.<app-domain>/<calendar-name> by default, and the
-- OWNER may instead give one calendar its own subdomain, which makes the account the first path
-- segment: <calendar-subdomain>.<app-domain>/<account-slug>/<calendar-name>. This table holds that
-- optional per-calendar subdomain (a single DNS label under the system domain), one row per
-- calendar.
--
-- Why a separate table rather than a column on booking_calendars: a calendar has at most one
-- subdomain, and the resolution is a lookup BY LABEL. A dedicated table means the label's unique
-- index and the calendar's one-row rule are both natural keys, and none of the existing
-- `SELECT * FROM booking_calendars` readers (BookingCalendar::from_row) change shape.
--
-- Uniqueness is the DATABASE's call, same as calendar_hosts.subdomain: one owner per label. The
-- cross-table rule — a calendar label must not collide with a workspace label — is enforced in the
-- handler (two tables cannot share one partial unique index), but the per-table index below means
-- no two calendars can hold one label regardless of what a handler misses.

CREATE TABLE IF NOT EXISTS calendar_subdomains (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    calendar_id UUID NOT NULL UNIQUE REFERENCES booking_calendars(id) ON DELETE CASCADE,
    subdomain TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT calendar_subdomains_subdomain_format
        CHECK (subdomain ~ '^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$'),
    CONSTRAINT calendar_subdomains_subdomain_not_system
        CHECK (subdomain NOT IN (
            'app','www','admin','api','apex','inbox','pmail','mail','email','smtp','imap','pop',
            'pop3','webmail','mx','mxa','mxb','ns1','ns2','ns','cdn','static','assets','media',
            'status','docs','help','support','billing','payments','checkout','calendar',
            'calendars','book','booking','bookings','crm','dashboard','login','logout','register',
            'signup','sign-up','account','accounts','auth','portal','blog','news','dev','staging',
            'test','staging1','origin','secure','vpn','ftp','whois','verify','downloads','files'
        ))
);

COMMENT ON TABLE calendar_subdomains IS
    'One optional per-calendar subdomain (kanban t_d4ac5433). A row here gives that calendar its own address <subdomain>.coreswiftcrm.com; the account slug becomes the first path segment.';

-- One owner per label, case-insensitively. The format CHECK already forces lowercase, but the
-- index is the thing a handler's write can never slip past.
CREATE UNIQUE INDEX IF NOT EXISTS calendar_subdomains_subdomain_uniq
    ON calendar_subdomains (lower(subdomain));
