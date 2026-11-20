-- 190_standing_default_currency.sql — the US DOLLAR is the standing default currency.
--
-- David's ruling, 2026-11-20 (relayed by Muse; kanban t_4e220c20), which supersedes the earlier
-- "a workspace with no default is asked to choose one, and its deals are refused until it does":
--
--   "The default currency will always be the US dollar. That is David's standing rule, not a guess
--    — so the 8 workspaces with no default currency get USD pre-set as their default. Nobody's
--    first deal attempt should be refused over a missing default. Deals can still be priced in any
--    currency per deal — this only sets the default."
--
-- So this migration seats `settings.default_currency = "USD"` on every workspace that has never
-- stated one. It is DATA, not a schema change: the resolution rule that makes it non-load-bearing
-- lives in `src/deals/fx.rs` (`STANDING_DEFAULT_CURRENCY` / `workspace_currencies`), so a workspace
-- created tomorrow is a dollar workspace even if no INSERT remembers to seat the key.
--
-- REPORTING CURRENCY IS DELIBERATELY NOT WRITTEN. `fx::workspace_currencies` derives reporting from
-- the default (`reporting_currency → default_currency → USD`), so a USD default already reports in
-- USD. Writing a second copy would make two stored facts that can disagree the moment a workspace
-- changes only its default.
--
-- IDEMPOTENT: a row that already states a currency is untouched, so a re-run is a no-op. A re-run
-- after someone has deliberately chosen EUR does not drag them back to USD.
--
-- NOT A BLANKET `jsonb_set`. `tenants.settings` is jsonb but not guaranteed to be an OBJECT — one
-- probe tenant carries a JSON string ("CNRY…"), and `jsonb_set` on a scalar is not the write you
-- think it is. The `jsonb_typeof(settings) = 'object'` guard means a non-object row is left exactly
-- as it is and simply reads as "stated nothing", which the standing default covers at read time.
-- Nothing else about any row is touched: no name, no slug, no plan, no probe flag, no new rows.

UPDATE tenants
   SET settings = jsonb_set(settings, '{default_currency}', to_jsonb('USD'::text), true),
       updated_at = NOW()
 WHERE jsonb_typeof(settings) = 'object'
   AND COALESCE(btrim(settings->>'default_currency'), '') = '';

-- A NULL settings cannot be patched in place; give it the smallest object that states the default.
-- (`tenants.settings` defaults to '{}', so this is for rows written before that default existed.)
UPDATE tenants
   SET settings = jsonb_build_object('default_currency', 'USD'),
       updated_at = NOW()
 WHERE settings IS NULL;
