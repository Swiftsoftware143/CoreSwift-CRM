-- 188_multi_currency_reporting.sql — MULTI-CURRENCY CORE, reporting layer.
--
-- David's amendment of 2026-11-20 (kanban t_44c1aaa4), on top of the deal writer (186):
--
--   * a workspace states TWO currencies: `default_currency` (what a new deal is priced in when
--     nobody says otherwise) and `reporting_currency` (what reports are computed in);
--   * every deal carries the rate it was recorded at AND the currency that rate converts INTO,
--     so a won deal's reported value is a stored fact, not a live market read;
--   * rates come from a table (`fx_rates`) refreshed on a schedule, one row per currency per day,
--     expressed as "units of this currency per 1 USD" — the cross rate is a division, which keeps
--     one source of truth instead of a pair matrix that can disagree with itself;
--   * money CUSTOM FIELDS get the same treatment: an amount, its own currency code and the same
--     stored rate, beside the display text the form shows.
--
-- Idempotent: IF NOT EXISTS / ON CONFLICT DO NOTHING everywhere, so a re-run is a no-op.

-- ── the rate table ────────────────────────────────────────────────────────────────────────────
-- `usd_rate` = units of `currency` per 1 USD (USD itself is 1). A cross rate is usd_rate(to) /
-- usd_rate(from), so two rows can never disagree about the same pair.
CREATE TABLE IF NOT EXISTS fx_rates (
    currency   text         NOT NULL,
    usd_rate   numeric(18,8) NOT NULL CHECK (usd_rate > 0),
    as_of      date         NOT NULL,
    source     text         NOT NULL DEFAULT 'seed',
    fetched_at timestamptz  NOT NULL DEFAULT now(),
    PRIMARY KEY (currency, as_of)
);
CREATE INDEX IF NOT EXISTS idx_fx_rates_latest ON fx_rates (currency, as_of DESC);

-- Baseline snapshot so the first deal of a brand-new workspace has a rate to store. These are
-- documented, approximate mid-market values as of 2026-11-20, source 'seed'; the daily refresh
-- (POST /api/internal/fx-rates/refresh, driven by n8n) replaces them and every later deal stores
-- the rate that was current on ITS day. Historical rows are never rewritten.
INSERT INTO fx_rates (currency, usd_rate, as_of, source) VALUES
    ('USD',     1.00000000, DATE '2026-11-20', 'seed'),
    ('EUR',     0.92000000, DATE '2026-11-20', 'seed'),
    ('GBP',     0.79000000, DATE '2026-11-20', 'seed'),
    ('CAD',     1.40000000, DATE '2026-11-20', 'seed'),
    ('MXN',    18.20000000, DATE '2026-11-20', 'seed'),
    ('BRL',     5.40000000, DATE '2026-11-20', 'seed'),
    ('JPY',   157.00000000, DATE '2026-11-20', 'seed'),
    ('AUD',     1.53000000, DATE '2026-11-20', 'seed'),
    ('CHF',     0.88000000, DATE '2026-11-20', 'seed'),
    ('INR',    84.50000000, DATE '2026-11-20', 'seed'),
    ('CNY',     7.25000000, DATE '2026-11-20', 'seed'),
    ('ZAR',    18.10000000, DATE '2026-11-20', 'seed'),
    ('SGD',     1.34000000, DATE '2026-11-20', 'seed'),
    ('NZD',     1.70000000, DATE '2026-11-20', 'seed'),
    ('SEK',    10.90000000, DATE '2026-11-20', 'seed'),
    ('NOK',    11.00000000, DATE '2026-11-20', 'seed'),
    ('DKK',     6.85000000, DATE '2026-11-20', 'seed'),
    ('PLN',     4.05000000, DATE '2026-11-20', 'seed'),
    ('AED',     3.67250000, DATE '2026-11-20', 'seed'),
    ('SAR',     3.75000000, DATE '2026-11-20', 'seed'),
    ('HKD',     7.78000000, DATE '2026-11-20', 'seed'),
    ('KRW',  1390.00000000, DATE '2026-11-20', 'seed'),
    ('TRY',    34.50000000, DATE '2026-11-20', 'seed'),
    ('NGN',  1680.00000000, DATE '2026-11-20', 'seed'),
    ('KES',   129.00000000, DATE '2026-11-20', 'seed'),
    ('GHS',    15.80000000, DATE '2026-11-20', 'seed'),
    ('COP',  4380.00000000, DATE '2026-11-20', 'seed'),
    ('ARS',  1010.00000000, DATE '2026-11-20', 'seed'),
    ('CLP',   970.00000000, DATE '2026-11-20', 'seed'),
    ('PEN',     3.78000000, DATE '2026-11-20', 'seed'),
    ('ILS',     3.72000000, DATE '2026-11-20', 'seed'),
    ('EGP',    49.00000000, DATE '2026-11-20', 'seed'),
    ('PHP',    58.80000000, DATE '2026-11-20', 'seed'),
    ('THB',    34.50000000, DATE '2026-11-20', 'seed'),
    ('IDR', 15900.00000000, DATE '2026-11-20', 'seed'),
    ('MYR',     4.45000000, DATE '2026-11-20', 'seed'),
    ('VND', 25400.00000000, DATE '2026-11-20', 'seed'),
    ('PKR',   278.00000000, DATE '2026-11-20', 'seed'),
    ('BDT',   119.00000000, DATE '2026-11-20', 'seed'),
    ('UAH',    41.30000000, DATE '2026-11-20', 'seed')
ON CONFLICT (currency, as_of) DO NOTHING;

-- ── what the deal's stored rate converts INTO ────────────────────────────────────────────────
-- Without this column an `exchange_rate` is a number with no destination: after a workspace
-- switches its reporting currency the stored rate would silently convert into the wrong unit.
ALTER TABLE opportunities ADD COLUMN IF NOT EXISTS reporting_currency text;

-- ── workspace settings ───────────────────────────────────────────────────────────────────────
-- A workspace that stated a default currency reports in that currency until it says otherwise.
UPDATE tenants t
   SET settings = jsonb_set(COALESCE(t.settings, '{}'::jsonb), '{reporting_currency}',
                            to_jsonb(t.settings ->> 'default_currency'), true)
 WHERE COALESCE(t.settings ->> 'default_currency', '') <> ''
   AND COALESCE(t.settings ->> 'reporting_currency', '') = '';

-- ── backfill existing deals ──────────────────────────────────────────────────────────────────
-- Every deal that already exists gets a rate and a rate date, using the latest rate on/after its
-- own currency's row. A same-currency deal stores exactly 1. A deal with no resolvable cross rate
-- (an exotic code the seed does not carry) is left alone rather than given an invented rate; the
-- reader counts those as unconverted instead of folding them into a total.
UPDATE opportunities o
   SET reporting_currency = upper(r.rep),
       exchange_rate = CASE
           WHEN upper(o.currency) = upper(r.rep) THEN 1
           ELSE ROUND(((SELECT usd_rate FROM fx_rates f WHERE upper(f.currency) = upper(r.rep)
                         ORDER BY f.as_of DESC LIMIT 1)
                     / (SELECT usd_rate FROM fx_rates f WHERE upper(f.currency) = upper(o.currency)
                         ORDER BY f.as_of DESC LIMIT 1))::numeric, 8)
       END,
       rate_date = COALESCE(o.rate_date, o.updated_at, now())
  FROM (
        SELECT t.id AS tenant_id,
               COALESCE(NULLIF(t.settings ->> 'reporting_currency', ''),
                        NULLIF(t.settings ->> 'default_currency', '')) AS rep
          FROM tenants t
       ) r
 WHERE o.tenant_id = r.tenant_id
   AND r.rep IS NOT NULL
   AND COALESCE(o.currency, '') <> ''
   AND (o.exchange_rate IS NULL OR o.reporting_currency IS NULL)
   AND (upper(o.currency) = upper(r.rep)
        OR (EXISTS (SELECT 1 FROM fx_rates f WHERE upper(f.currency) = upper(r.rep))
            AND EXISTS (SELECT 1 FROM fx_rates f WHERE upper(f.currency) = upper(o.currency))));

-- ── money custom fields ─────────────────────────────────────────────────────────────────────
-- The display text stays in `value` (the form shows it); the money ITSELF lives in typed columns
-- so a report can add it up without parsing a string, and it keeps the same stored-rate contract
-- as the deal's own value (David: "money-type custom fields obey the same rules").
ALTER TABLE deal_field_values
    ADD COLUMN IF NOT EXISTS money_amount numeric(18,2),
    ADD COLUMN IF NOT EXISTS money_currency text,
    ADD COLUMN IF NOT EXISTS money_exchange_rate numeric(18,8),
    ADD COLUMN IF NOT EXISTS money_rate_date timestamptz,
    ADD COLUMN IF NOT EXISTS money_reporting_currency text;

CREATE INDEX IF NOT EXISTS idx_deal_field_values_money
    ON deal_field_values (money_currency)
    WHERE money_amount IS NOT NULL;
