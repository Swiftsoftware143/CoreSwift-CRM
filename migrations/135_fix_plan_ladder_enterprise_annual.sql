-- kanban t_e27f484b (pricing hygiene): the sold ladder had two packaging faults measured on the
-- live `plans` table on 2026-11-11.
--
-- (1) EQUAL PRICE, STRICT SUPERSET. `professional` (sort 2) and `enterprise` (sort 3) were BOTH
--     $79/month and BOTH active, while enterprise is a strict superset of professional — 20 seats
--     vs 15, 10,000 contacts vs 5,000, 20 pipelines vs 5, 10 integrations vs 5, 7 support widgets,
--     email campaigns, branded templates and the ticket inbox on top. At an identical price nobody
--     can sell Professional: the app's own upgrade screen offers the better tier for the same money.
--
-- (2) ANNUAL BILLED AS A PENALTY. enterprise carried price_yearly = 1990 against a 79/month rate:
--     1990 / 79 = 25.2 months of service for a year's money. Every other sold plan bills its year
--     at exactly 10 months (290/29, 790/79, 1490/149). That is not a discount, it is a fine.
--
-- REPAIR. enterprise moves to $119/month and $1,190/year: strictly above professional ($79) so the
-- tier is sellable, strictly below the top tier `agency` ($149 / $1,490), and its year is exactly
-- 10 months like every other plan. `is_active` is not touched — enterprise stays sold.
--
-- NO CUSTOMER'S BILL CHANGES. Measured at repair time: active subscriptions were free = 2,
-- agency = 5, and starter / pro / professional / enterprise = 0. Nothing is repriced under an
-- existing subscriber.
--
-- `pro` IS LEFT INACTIVE ON PURPOSE. It is the legacy $29 sibling that module gating and the
-- FunnelSwift hand-off still name by slug (migration 114), but `plans.is_active = false` means it is
-- not sold, so it cannot undercut Starter's identical $29. Retiring the row would break the gating
-- vocabulary; leaving it active would re-create the Starter defect. Inactive is the correct state.
--
-- Idempotent and guarded on the exact broken pair, so a re-run (or a fresh from-zero build whose
-- migration 024 seed already writes a coherent 199/1990) is a no-op and never fights a price the
-- owner later sets from the admin panel.

UPDATE plans
   SET price_monthly = 119.00,
       price_yearly  = 1190.00,
       updated_at    = NOW()
 WHERE slug = 'enterprise'
   AND price_monthly = 79.00
   AND price_yearly = 1990.00;

-- Guard the invariant this repair restores, so the fault cannot be re-introduced silently by a
-- later data edit: every ACTIVE plan above the free tier must bill its year at 10 months.
DO $$
DECLARE
    bad int;
BEGIN
    SELECT count(*) INTO bad
      FROM plans
     WHERE is_active
       AND price_monthly > 0
       AND abs(price_yearly - price_monthly * 10) > 0.01;
    IF bad > 0 THEN
        RAISE EXCEPTION 'plan annual billing is not 10 months for % active plan(s)', bad;
    END IF;
END $$;
