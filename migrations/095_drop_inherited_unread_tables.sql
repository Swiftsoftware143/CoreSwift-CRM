-- 095_drop_inherited_unread_tables.sql
-- CoreSwift-CRM: DROP the 13 tables that exist here only as copies of a SIBLING app's schema.
-- Decision card t_d3e0bab6 (the drop-vs-keep question t_d8b2e888 deferred).  DECISION: DROP.
--
-- 092 gave these 15 no-DDL tables their live definition so a database built from this repository
-- equals the live one.  That goal is unchanged by this file: 093..094 untouched, and from-zero now
-- runs 092 (create) then 095 (drop) and still lands exactly on live.
--
-- WHY DROP rather than keep-as-roadmap (evidence in /opt/swift/audits/t_d3e0bab6/):
--   1. NO READER, EVER.  Not one commit of this repository ever named any of the 13 in code
--      (`git log --all -S'<name>' -- src www` = 0 for 12 of them; the only `api_keys` hits are the
--      local `let mut api_keys = HashMap` in src/ai/handlers.rs:245).  No SQL statement in src/ or
--      www/ targets any of them, and no view/function/trigger/default in the database mentions them.
--   2. THE ROADMAP FEATURES ALREADY EXIST HERE UNDER OTHER NAMES, LIVE AND READ:
--      per-plan tiering = plans (81 src refs) + tenant_plans (37) + plan_modules/plan_module_features
--      (11/12); API keys = personal_api_keys (9) + private_email_api_keys (10); tag automation =
--      tags (125) + tag_categories + tag_mappings + tag_assignments (35) + automation_rules (16);
--      SEO = admin_settings (src/admin_actions/site_handler.rs).  Keeping a second, unread
--      `plan_tiers`/`tag_rules` is a trap: it is the table a future lane would write by mistake.
--   3. THEY ARE COPIES OF ANOTHER APP'S TABLES, SOME OF THEM STALE.  Shape-identical to
--      multi_directory (ZaarHub): api_keys, api_key_usage, business_subscriptions, city_plan_slots,
--      network_branding, legal_pages, google_places_cache, seo_meta.  Strict SUBSETS of the
--      sibling's current shape (i.e. copies taken before the sibling evolved): plan_tiers 25 of 31,
--      networks 14 of 19 (the sibling added coreswift_base_url / coreswift_list_id_businesses /
--      _suppliers / _users / coreswift_personal_key_encrypted to ITS copy - if CoreSwift owned
--      `networks`, those CoreSwift-namespaced columns would have appeared here).  Different
--      generation of the same template: payment_providers, payment_webhook_events (ADASwift
--      checkout_handler.rs reads/writes its own), tag_rules (FunnelSwift tag_logic.rs /
--      tag_rule_handler.rs read/write their own).
--   4. THE REAL HOMES HOLD THE DATA AND THE READERS: multi_directory carries city_plan_slots 30,
--      google_places_cache 4482, seo_meta 381, legal_pages 5, plan_tiers 3, networks 3,
--      network_branding 2, payment_webhook_events 1; here every one of the 13 has 0 rows except
--      legal_pages (1 row = a copy of IncentiveSwift's terms, seeded into multi_directory by its
--      028_seed_incentiveswift_terms.sql).
--   5. SAFE TO DROP: 0 inbound FK edges from outside the group, so CASCADE removes nothing else;
--      only crm-swift connects to this database; no n8n workflow and no pending board card for this
--      app names any of them.  Every table was pg_dump'd (schema+data) to
--      /opt/swift/audits/t_d3e0bab6/dumps/<table>.sql plus a full -Fc dump of the database, and 092
--      still holds the exact live definition, so any of them is one migration away from returning.
--
-- Idempotent: IF EXISTS, so this is a no-op on a database that never had them.  No BEGIN/COMMIT:
-- the sqlx runner already wraps each file in one transaction (no other file in this directory
-- declares its own), and children are dropped before their parents, so no CASCADE is needed.

DROP TABLE IF EXISTS api_key_usage;          -- child of api_keys
DROP TABLE IF EXISTS api_keys;
DROP TABLE IF EXISTS network_branding;       -- child of networks
DROP TABLE IF EXISTS networks;
DROP TABLE IF EXISTS city_plan_slots;        -- child of plan_tiers
DROP TABLE IF EXISTS business_subscriptions; -- child of plan_tiers
DROP TABLE IF EXISTS plan_tiers;
DROP TABLE IF EXISTS legal_pages;
DROP TABLE IF EXISTS seo_meta;
DROP TABLE IF EXISTS google_places_cache;
DROP TABLE IF EXISTS tag_rules;
DROP TABLE IF EXISTS payment_webhook_events;
DROP TABLE IF EXISTS payment_providers;
