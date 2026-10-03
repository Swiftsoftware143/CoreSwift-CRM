-- CoreSwift — retire the CheatLayer connector everywhere it was seeded.
--
-- David, 2026-10-03: *"Cheat Layer is unnecessary. You can eliminate that out of any of the apps. Will
-- revisit how we're going to use that later."*
--
-- MEASURED BEFORE TOUCHING ANYTHING (live, 2026-10-03):
--   * `native_apps`          — 1 catalogue row (slug 'cheatlayer'), seeded by migration 025. This is the
--                              list the connector screen renders.
--   * `available_providers`  — 1 row (key 'cheatlayer'), seeded by migration 069, which the Integration
--                              Center renders "instead of a hardcoded array in the SPA" (069's own words).
--                              Leaving it would keep offering a connector that no longer exists.
--   * `modules.description`  — the 'native_apps' module row still advertises it, and that description is
--                              what the plan/feature matrix shows an operator.
--   * `app_connections`      — ZERO rows, so NO tenant had ever connected it and no credentials are lost.
--                              That is the check that makes deleting right instead of deactivating: there is
--                              nothing configured to preserve.
--   * Rust side              — the registry entry, the `pub mod`, the four dispatch arms (test/push/pull/
--                              get_meta) and connectors/cheatlayer.rs are removed in the same commit. A
--                              catalogue row with no implementation, or an implementation with no row, is
--                              exactly how those two drift apart.
--
-- A NEW migration, not an edit: 025/069/072 are APPLIED and editing an applied migration aborts boot on the
-- checksum. Undoing the seeds here in order also means a FRESH install never carries the connector, which
-- deactivating a row would not achieve.
--
-- RE-ADDING LATER (David: "will revisit how we're going to use that later") needs a connector module + a
-- registry entry + the four dispatch arms + a catalogue row + a provider row. Nothing here is
-- load-bearing for any other connector: the remaining five keep their rows, their keys and their dispatch.

DELETE FROM native_apps WHERE slug = 'cheatlayer';
DELETE FROM available_providers WHERE key = 'cheatlayer';

UPDATE modules
   SET description = 'FunnelSwift, ADASwift, MissedCall, WorkflowSwift, Multi-Directory',
       updated_at  = NOW()
 WHERE key = 'native_apps';
