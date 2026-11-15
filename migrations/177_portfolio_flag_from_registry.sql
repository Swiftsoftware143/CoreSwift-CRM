-- `tenants.is_portfolio` is set FROM the registry — the flag and the list can no longer disagree.
--
-- Measured live 2026-11-15 (kanban t_e2517c18): `portfolio_companies` — the registry the hub's own
-- portfolio sync writes — held FIVE companies (BV Premiere Products, Giraudy Capital, SwiftImpact
-- Solutions, Xride Drivers, ZaarHub) while `tenants.is_portfolio` was true for exactly ONE of them
-- (Xride Drivers). BV Premiere Products therefore carried neither the flag nor an owner marker, so a
-- single `DELETE /api/account/<id>` from the console passed every guard and destroyed an owner row.
--
-- The delete guards now read this registry directly (`tenant_purge::portfolio_registry_hit`), so the
-- protection no longer depends on this flag at all. The flag is still read by the admin panel's
-- tenant list and by the workspace delete door, so it is made honest here — and
-- `portfolio::provision` now sets it on every sync, so a company mirrored tomorrow arrives flagged.
--
-- Matched on the link column the provisioning pass writes (`account_tenant_id`) AND on the name and
-- slug, so a company whose link column is still NULL is fixed by this statement too.
UPDATE tenants t
   SET is_portfolio = true
  FROM portfolio_companies c
 WHERE (c.account_tenant_id = t.id
        OR lower(c.name) = lower(t.name)
        OR lower(c.slug) = lower(t.slug))
   AND COALESCE(t.is_portfolio, false) = false;
