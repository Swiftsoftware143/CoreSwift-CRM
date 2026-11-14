-- 159_booking_name_per_workspace.sql
--
-- A calendar's name is unique per WORKSPACE, not platform-wide.
--
-- Migration 156 made `booking_calendars.slug` globally unique so the shared address could resolve a
-- bare name. That is the wrong constraint for the product: "consultations" is an obvious name and two
-- owners are allowed to want it — one of them losing it to a stranger is a support ticket and a reason
-- to leave.
--
-- Uniqueness per workspace already exists (`booking_calendars_tenant_id_slug_key`), so the global
-- index is dropped here. Resolution stays honest: a bare name is accepted only while it is
-- unambiguous, and a name held by more than one workspace must arrive workspace-qualified — the shared
-- address carries it in the path (`/b/<workspace>/<name>`, passed to the API as `?w=`), and a
-- workspace host or a verified own domain implies the workspace by host.

drop index if exists idx_booking_calendars_slug_global;

-- keep the per-workspace rule enforced by the database, named so its purpose is obvious
create unique index if not exists idx_booking_calendars_ws_slug
    on booking_calendars (tenant_id, slug);

comment on column booking_calendars.slug is
    'Address name, unique per workspace. A bare name is only resolvable while unambiguous; two workspaces may hold the same name, so the shared address qualifies it with the workspace.';
