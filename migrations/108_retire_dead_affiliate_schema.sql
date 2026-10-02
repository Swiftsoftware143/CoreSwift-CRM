-- CoreSwift — retire the DEAD AFFILIATE SCHEMA: five tables that were schema and nothing else.
--
-- David, 2026-10-02: *"get rid of what's not needed."* This is that, in the one place CoreSwift should
-- never have had it. The affiliate programme is OWNED by FunnelSwift and used by every app
-- (ARCHITECTURE.md Rule 5 — loyalty points and affiliate are separate systems, and the affiliate side
-- belongs to FunnelSwift). A second, empty affiliate schema living in CoreSwift is exactly the
-- duplication that rule exists to prevent, and it reads to the next reader as a feature that exists.
--
-- MEASURED 2026-10-02, before anything was touched:
--   * `affiliates`, `commission_payouts`, `referrals`, `affiliate_products`,
--     `affiliate_product_selections` — ALL FIVE have 0 rows in `coreswift_crm`.
--   * 0 registered routes (nothing in main.rs), 0 reads (`FROM <table>`), 0 writes
--     (`INSERT`/`UPDATE`/`DELETE`) anywhere in `src/`.
--   * the ONE source mention is `src/native_apps/connectors/funnelswift.rs`, which DECLARES
--     `affiliate_products` as a pull entity from FunnelSwift. That is an unimplemented capability, not a
--     consumer of this schema — no sync code exists — so dropping the table cannot break it. Whether the
--     sync gets built or the declaration comes out is carded separately: t_7225a94d.
--   * the served guide's affiliate copy is CORRECT AND STAYS. It tells the reader the affiliate system is
--     managed in FunnelSwift. No prose is touched by this file.
--
-- WHY DROP RATHER THAN LEAVE AN EMPTY TABLE ALONE: an empty table is not inert. It is copied into the
-- next fresh install, it is read by the next person as a feature, and it invites someone to build the
-- second affiliate system this app is supposed to be without.
--
-- SAFETY — THIS CANNOT DESTROY DATA IT DID NOT MEASURE. Every drop is guarded on the table being EMPTY
-- at apply time; if any of them has even one row the statement raises and the migration ABORTS.
DO $$
DECLARE
    t text;
    n bigint;
BEGIN
    FOREACH t IN ARRAY ARRAY[
        'affiliate_product_selections',
        'affiliate_products',
        'commission_payouts',
        'referrals',
        'affiliates'
    ] LOOP
        IF EXISTS (
            SELECT 1 FROM information_schema.tables
             WHERE table_schema = 'public' AND table_name = t
        ) THEN
            EXECUTE format('SELECT count(*) FROM %I', t) INTO n;
            IF n > 0 THEN
                RAISE EXCEPTION
                    'refusing to drop %: it holds % row(s). This migration was measured against an EMPTY table; a populated one means something now uses it and it must be wired or retired deliberately.',
                    t, n;
            END IF;
            EXECUTE format('DROP TABLE IF EXISTS %I CASCADE', t);
        END IF;
    END LOOP;
END $$;
