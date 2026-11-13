-- One active API key per account.
--
-- The key is minted in the SAME transaction as the tenant (auth::signup::create_account), so a
-- workspace can never exist without one. A workspace may hold only ONE ACTIVE key: the account's
-- key, which the sibling apps and the MCP client both use.
--
-- Per-app keys (one CoreSwift-minted key for each sibling app) were retired on 2026-11-13: the
-- correct model is the reverse — CoreSwift HOLDS each sibling app's own key to reach out to it,
-- and every sibling uses this ONE account key to reach in.
--
-- The partial unique index is the database-level guarantee: a retry, a racing signup, or a future
-- writer cannot leave two active keys for one account. Inactive (revoked/rotated) rows are
-- unaffected by the predicate, so history survives a Regenerate.
--
-- `personal_api_keys.create_key` now refuses when an active key already exists, and
-- `rotate_key` retires the old row inside the same transaction as the mint, so neither path can
-- trip this index.
CREATE UNIQUE INDEX IF NOT EXISTS idx_personal_api_keys_one_active
    ON personal_api_keys (tenant_id)
 WHERE is_active;
