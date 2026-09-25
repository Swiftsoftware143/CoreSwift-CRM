-- 090_retire_dangling_tenant_rows_validate_fks.sql
-- kanban t_9dc0eb64 (the cleanup card 088's header calls t_9a4ac01d; that id was a forward-reference
-- placeholder written before the card existed — the real card is t_9dc0eb64 and 088 is applied, so it
-- cannot be re-edited).
--
-- 088 armed the 7 missing `tenant_id` references to `tenants`. Five could be validated immediately;
-- two could not, because their tables still held the backlog the missing FK had allowed:
--
--   outbound_messages    383 rows whose tenant no longer exists (297 failed, 86 sent / 359 deleted
--                        tenant ids, 2026-08-09 .. 2026-09-23)   convalidated=false
--   portfolio_companies    3 rows, tenant 869c33e6 (ZaarHub / Giraudy Capital / SwiftImpact
--                        Solutions, 2026-08-08)                  convalidated=false
--
-- Arm chosen: apply the delete action 088 already declares to that historical backlog, then validate.
-- Evidence for DELETE-vs-archive-vs-repoint, measured live 2026-09-25 (see
-- /opt/swift/audits/cs-tenant-fk-cleanup-t_9dc0eb64/00-baseline.txt and REPORT.md):
--   * 088 declares `ON DELETE CASCADE` on both, i.e. `outbound_messages`/`portfolio_companies` rows
--     are tenant-owned data that dies WITH its workspace. A row the armed FK would have deleted at
--     delete time is not information the schema wants to keep; leaving it is the defect 088 closed.
--   * every live reader of outbound_messages filters by tenant_id (src/communications/handlers.rs,
--     src/ai/engine.rs), and the send worker only picks up status='queued' — 0 of the 383 are
--     queued/sending, so no send can be affected by retiring terminal (failed/sent) rows. The only
--     non-per-tenant reader is the retention sweep get_purge_targets(), which reads DISTINCT
--     tenant_id FROM outbound_messages: 359 of its 383 inputs are tenants that do not exist, so the
--     retirement also makes that loop's worklist truthful. The table is declared ephemeral by the
--     app itself (src/private_email/purge.rs: default 365-day retention, min 30); nothing re-pointed
--     or archived is readable by anybody afterwards.
--   * portfolio_companies holds 3 rows in total and 3 are dangling: 100% of the table is unreachable
--     (no reader can name a tenant for it, and 0 rows in integration_targets reference them), and no
--     `is_portfolio` tenant exists to re-point them at. Inventing an owner would make deleted-workspace
--     rows look like a live workspace's portfolio.
--   * Re-point-to-NULL was rejected: outbound_messages.tenant_id is nullable, but NULL destroys the
--     only record of which workspace sent the row while being no more readable than deletion.
--   * Both tables were backed up first — exact rows as JSON + replayable INSERTs:
--     01-backup-outbound_messages.{json,sql} (383), 01-backup-portfolio_companies.{json,sql} (3),
--     id sets + sha256 in 01-backup-manifest.json. The restore path is exercised, not assumed.
--
-- Step 2 is the point of the card: the two constraints flip to convalidated=true.
-- Step 3 indexes business_profiles.tenant_id — the only one of the 7 tables of 088 with no index whose
-- LEADING column is tenant_id, so the FK's cascade lookup (`DELETE FROM tenants` -> child prune) had
-- to seq-scan it. account_health needs nothing: account_health_tenant_entity_key is a UNIQUE btree
-- (tenant_id, entity_type, entity_id), so its lookup already uses that index.
--
-- Measured live in ONE BEGIN .. ROLLBACK before deploy (02-rehearsal.txt): both constraints
-- convalidated=true inside the transaction, outbound_messages 498 -> 115, portfolio_companies 3 -> 0,
-- business_profiles relfilenode 25306 -> 25306 (CREATE INDEX does not rewrite the heap; 7.7 ms on a
-- 2-row table), cascade plans show `Index Scan using account_health_tenant_entity_key` and
-- `Index Scan using idx_business_profiles_tenant` under enable_seqscan=off, and the whole-database
-- fingerprint (119 tables / 4233 rows / md5 1ff838c4…) is identical before and after the ROLLBACK —
-- i.e. nothing live moved.
--
-- Re-running this file is a no-op: 0 rows match the deletes, VALIDATE on a validated constraint
-- succeeds silently, and the index is created IF NOT EXISTS.

-- 1. Retire the rows whose tenant resolves to nothing, so the constraints can be validated. The
--    counts land in the boot log (RAISE NOTICE) as the deploy's own record of what was removed.
DO $$
DECLARE
    n_outbound bigint;
    n_portfolio bigint;
BEGIN
    DELETE FROM outbound_messages
     WHERE tenant_id IS NOT NULL
       AND tenant_id NOT IN (SELECT id FROM tenants);
    GET DIAGNOSTICS n_outbound = ROW_COUNT;

    DELETE FROM portfolio_companies
     WHERE tenant_id NOT IN (SELECT id FROM tenants);
    GET DIAGNOSTICS n_portfolio = ROW_COUNT;

    RAISE NOTICE 't_9dc0eb64: retired % dangling outbound_messages and % dangling portfolio_companies rows (tenant no longer exists)', n_outbound, n_portfolio;
END $$;

-- 2. Validate the two references 088 had to arm NOT VALID. Both tables are free of violations now,
--    so this is a pass over the remaining rows only (115 + 0 on live).
ALTER TABLE outbound_messages VALIDATE CONSTRAINT outbound_messages_tenant_id_fkey;
ALTER TABLE portfolio_companies VALIDATE CONSTRAINT portfolio_companies_tenant_id_fkey;

-- 3. The cascade lookup index business_profiles was missing (see header). No table rewrite: CREATE
--    INDEX builds a new relation only, the heap's relfilenode is unchanged (measured).
CREATE INDEX IF NOT EXISTS idx_business_profiles_tenant ON business_profiles (tenant_id);
