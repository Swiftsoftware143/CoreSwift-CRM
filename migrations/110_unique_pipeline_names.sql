-- CoreSwift — one pipeline per name per tenant, and the guard that was missing.
--
-- MEASURED 2026-10-02 (live). Tenant abd8ad22-aa01-4642-9a9f-6bef6a03d85b owned TWO pipelines with the
-- identical name "Sales Pipeline", created 2026-06-26 and 2026-06-29 — three days apart, so the default
-- pipeline seeding ran more than once and neither run could see the other's work.
--
-- WHY IT MATTERS TO A CUSTOMER: the pipeline picker offers the operator the same pipeline twice, and the
-- stage lists union into apparent duplicates ("Won" x3, "Contacted" x3). Worth recording how I got that
-- wrong first: I grouped stages by name WITHOUT pipeline_id, which made two pipelines look like duplicated
-- STAGES. No single pipeline has a duplicate stage name — verified. The duplication is at the PIPELINE
-- level. A query that forgets the id it is grouping by will invent a bug that isn't there.
--
-- THE GUARD WAS ABSENT, not just unused: pg_indexes on `pipelines` showed only the primary key, so nothing
-- prevented the next seeding run from adding a third. That is the part that must not recur — the specific
-- rows below are only the ones this ran into.
--
-- SAFETY, stated because a DELETE like this must be justifiable:
--   * a pipeline is removed ONLY when it has NO opportunities, so nothing cascades a customer's deals away
--     (opportunities.pipeline_id is ON DELETE CASCADE, which is exactly why this checks first);
--   * the EARLIEST twin is kept, so ids already referenced elsewhere keep working;
--   * after this, the unique index cannot fail to build.
--
-- STILL OPEN, deliberately: a tenant with two same-named pipelines whose LATER twin HOLDS deals is left
-- alone — choosing which of two live pipelines is authoritative is a product decision, and the index below
-- will refuse to build if such a pair exists, which is the correct loud failure rather than a silent pick.
-- Carded as t_d225e221.

-- 1. drop the stages of any pipeline that is about to be removed, so nothing is orphaned mid-way
DELETE FROM pipeline_stages s
 WHERE EXISTS (
   SELECT 1 FROM pipelines p
    WHERE p.id = s.pipeline_id
      AND NOT EXISTS (SELECT 1 FROM opportunities o WHERE o.pipeline_id = p.id)
      AND EXISTS (
        SELECT 1 FROM pipelines k
         WHERE k.tenant_id = p.tenant_id
           AND lower(k.name) = lower(p.name)
           AND (k.created_at, k.id) < (p.created_at, p.id)
      )
 );

-- 2. remove the redundant, deal-free twins (the earliest one survives)
DELETE FROM pipelines p
 WHERE NOT EXISTS (SELECT 1 FROM opportunities o WHERE o.pipeline_id = p.id)
   AND EXISTS (
     SELECT 1 FROM pipelines k
      WHERE k.tenant_id = p.tenant_id
        AND lower(k.name) = lower(p.name)
        AND (k.created_at, k.id) < (p.created_at, p.id)
   );

-- 3. the guard: case-insensitive, per tenant. A re-seed can no longer create a second "Sales Pipeline",
--    and the create path answers a clean 400 on the conflict rather than a raw 500 (see create_pipeline).
CREATE UNIQUE INDEX IF NOT EXISTS idx_pipelines_tenant_name_unique
    ON pipelines (tenant_id, lower(name));
