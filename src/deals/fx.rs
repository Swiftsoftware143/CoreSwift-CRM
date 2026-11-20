//! The multi-currency core: ISO 4217, the workspace's two currencies, and the rate table.
//!
//! David's amendment, 2026-11-20 (kanban t_44c1aaa4): a money value is an AMOUNT PLUS A CURRENCY
//! CODE, a workspace states a default and a reporting currency, and the rate a deal was recorded
//! at is stored WITH the deal so a past quarter does not move when the market does.
//!
//! ## One source of truth for rates
//! `fx_rates` holds one row per currency per day, as **units of that currency per 1 USD**. A cross
//! rate is `usd_rate(to) / usd_rate(from)`, so a pair can never disagree with itself the way a
//! pair-matrix does. `refresh_rates` pulls the day's snapshot from a free, key-less provider and
//! is driven by the fleet's n8n cron through `POST /api/internal/fx-rates/refresh`.
//!
//! ## What is stored on the deal
//! `exchange_rate` converts **1 unit of the deal's currency into the reporting currency**, and
//! `rate_date` is the day that rate was true. `reporting_currency` names the destination, because
//! a rate with no stated destination converts into the wrong unit after a workspace changes its
//! reporting currency. Reports read the STORED rate, never the table (that is the whole point).
//!
//! A workspace with no rate for a currency pair is refused with a sentence naming both remedies
//! (send `exchange_rate` with the deal, or refresh the rate table) — never given a guessed rate,
//! and never left with a rate-less money value.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::errors::AppError;

/// The ISO 4217 currency codes this app accepts. Not free text: a three-letter code that is not a
/// real currency is refused, because "XXX" or "ABC" would store a figure nothing can convert.
pub const ISO_4217: &[&str] = &[
    "AED", "AFN", "ALL", "AMD", "ANG", "AOA", "ARS", "AUD", "AWG", "AZN", "BAM", "BBD", "BDT",
    "BGN", "BHD", "BIF", "BMD", "BND", "BOB", "BRL", "BSD", "BTN", "BWP", "BYN", "BZD", "CAD",
    "CDF", "CHF", "CLP", "CNY", "COP", "CRC", "CUP", "CVE", "CZK", "DJF", "DKK", "DOP", "DZD",
    "EGP", "ERN", "ETB", "EUR", "FJD", "FKP", "GBP", "GEL", "GHS", "GIP", "GMD", "GNF", "GTQ",
    "GYD", "HKD", "HNL", "HTG", "HUF", "IDR", "ILS", "INR", "IQD", "IRR", "ISK", "JMD", "JOD",
    "JPY", "KES", "KGS", "KHR", "KMF", "KPW", "KRW", "KWD", "KYD", "KZT", "LAK", "LBP", "LKR",
    "LRD", "LSL", "LYD", "MAD", "MDL", "MGA", "MKD", "MMK", "MNT", "MOP", "MRU", "MUR", "MVR",
    "MWK", "MXN", "MYR", "MZN", "NAD", "NGN", "NIO", "NOK", "NPR", "NZD", "OMR", "PAB", "PEN",
    "PGK", "PHP", "PKR", "PLN", "PYG", "QAR", "RON", "RSD", "RUB", "RWF", "SAR", "SBD", "SCR",
    "SDG", "SEK", "SGD", "SHP", "SLE", "SOS", "SRD", "SSP", "STN", "SVC", "SYP", "SZL", "THB",
    "TJS", "TMT", "TND", "TOP", "TRY", "TTD", "TWD", "TZS", "UAH", "UGX", "USD", "UYU", "UZS",
    "VES", "VND", "VUV", "WST", "XAF", "XCD", "XOF", "XPF", "YER", "ZAR", "ZMW", "ZWL",
];

pub fn is_iso_currency(code: &str) -> bool {
    ISO_4217.contains(&code)
}

/// Trim, uppercase and validate a caller-supplied code against the real list.
pub fn normalize_iso(raw: &str) -> Result<String, AppError> {
    let up = raw.trim().to_uppercase();
    if up.len() != 3 || !up.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(AppError::Validation(format!(
            "\"{raw}\" is not a currency code — send a 3-letter ISO 4217 code, e.g. USD, EUR, GBP"
        )));
    }
    if !is_iso_currency(&up) {
        return Err(AppError::Validation(format!(
            "{up} is not an ISO 4217 currency code"
        )));
    }
    Ok(up)
}

/// The workspace's stated currency pair: (default, reporting).
pub async fn workspace_currencies(
    db: &PgPool,
    tenant_id: Uuid,
) -> Result<(Option<String>, Option<String>), AppError> {
    let settings: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT settings FROM tenants WHERE id = $1")
            .bind(tenant_id)
            .fetch_optional(db)
            .await?;
    let pick = |s: &serde_json::Value, keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|k| s.get(k).and_then(|v| v.as_str()).map(str::to_string))
            .map(|c| c.trim().to_uppercase())
            .filter(|c| !c.is_empty())
    };
    Ok(match settings {
        Some(s) => (
            pick(&s, &["default_currency", "currency"]),
            pick(&s, &["reporting_currency", "default_currency", "currency"]),
        ),
        None => (None, None),
    })
}

/// What reports are computed in. Falls back to the default currency, because a workspace that has
/// chosen ONE currency has chosen both.
pub async fn workspace_reporting_currency(
    db: &PgPool,
    tenant_id: Uuid,
) -> Result<Option<String>, AppError> {
    Ok(workspace_currencies(db, tenant_id).await?.1)
}

/// Latest rate for a currency on or before `as_of` (or the latest overall when omitted):
/// `(units of that currency per 1 USD, the day it was true)`.
pub async fn usd_rate(
    db: &PgPool,
    currency: &str,
    as_of: Option<NaiveDate>,
) -> Result<Option<(f64, NaiveDate)>, AppError> {
    let row = sqlx::query(
        "SELECT usd_rate::float8 AS usd_rate, as_of FROM fx_rates
          WHERE upper(currency) = upper($1) AND ($2::date IS NULL OR as_of <= $2::date)
          ORDER BY as_of DESC LIMIT 1",
    )
    .bind(currency)
    .bind(as_of)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|r| (r.get::<f64, _>("usd_rate"), r.get::<NaiveDate, _>("as_of"))))
}

/// The cross rate `from -> to`, rounded to 8 places, with the day it was quoted.
///
/// Same currency is 1 by definition and does not need a table row. The rate date reported is the
/// OLDER of the two quotations, because a cross rate is only true when both legs are.
pub async fn rate_between(
    db: &PgPool,
    from: &str,
    to: &str,
    as_of: Option<NaiveDate>,
) -> Result<Option<(f64, NaiveDate)>, AppError> {
    if from.eq_ignore_ascii_case(to) {
        return Ok(Some((
            1.0,
            as_of.unwrap_or_else(|| Utc::now().date_naive()),
        )));
    }
    let (Some((from_rate, from_day)), Some((to_rate, to_day))) = (
        usd_rate(db, from, as_of).await?,
        usd_rate(db, to, as_of).await?,
    ) else {
        return Ok(None);
    };
    if from_rate <= 0.0 {
        return Ok(None);
    }
    let rate = (to_rate / from_rate * 100_000_000.0).round() / 100_000_000.0;
    Ok(Some((rate, from_day.min(to_day))))
}

/// Store the workspace's currency choice. `reporting` is what reports compute in; when a workspace
/// sets only a default, the reporting currency follows it (one currency = one answer).
pub async fn set_workspace_currencies(
    db: &PgPool,
    tenant_id: Uuid,
    default: Option<&str>,
    reporting: Option<&str>,
) -> Result<(Option<String>, Option<String>), AppError> {
    if let Some(d) = default {
        sqlx::query(
            "UPDATE tenants SET settings = jsonb_set(COALESCE(settings,'{}'::jsonb),
                                                     '{default_currency}', to_jsonb($2::text), true),
                    updated_at = NOW() WHERE id = $1",
        )
        .bind(tenant_id)
        .bind(d)
        .execute(db)
        .await?;
    }
    if let Some(r) = reporting {
        sqlx::query(
            "UPDATE tenants SET settings = jsonb_set(COALESCE(settings,'{}'::jsonb),
                                                     '{reporting_currency}', to_jsonb($2::text), true),
                    updated_at = NOW() WHERE id = $1",
        )
        .bind(tenant_id)
        .bind(r)
        .execute(db)
        .await?;
    }
    // A workspace that names a default but never names a reporting currency reports in its
    // default; keeping that rule in one place means no reader has to guess.
    if default.is_some() && reporting.is_none() {
        let (_, current) = workspace_currencies(db, tenant_id).await?;
        if current.is_none() {
            sqlx::query(
                "UPDATE tenants SET settings = jsonb_set(COALESCE(settings,'{}'::jsonb),
                                                         '{reporting_currency}', to_jsonb($2::text), true)
                  WHERE id = $1",
            )
            .bind(tenant_id)
            .bind(default.unwrap())
            .execute(db)
            .await?;
        }
    }
    workspace_currencies(db, tenant_id).await
}

/// Re-quote every deal in a workspace into `reporting`, using the rate that was true on each
/// deal's OWN `rate_date`.
///
/// This runs only when the workspace CHANGES its reporting currency — never when the market moves
/// — which is what keeps a past won deal's reported value stable (David's condition 2). A deal
/// whose historical rate cannot be found keeps its stored rate and is counted as unconverted
/// rather than being re-priced from today's market.
pub async fn requote_tenant(
    db: &PgPool,
    tenant_id: Uuid,
    reporting: &str,
) -> Result<u64, AppError> {
    let rows = sqlx::query(
        "SELECT id, currency, rate_date::date AS day FROM opportunities
          WHERE tenant_id = $1 AND COALESCE(currency,'') <> ''
            AND upper(currency) <> upper($2)",
    )
    .bind(tenant_id)
    .bind(reporting)
    .fetch_all(db)
    .await?;

    let mut updated = 0u64;
    for row in rows {
        let id: Uuid = row.get("id");
        let currency: String = row.get("currency");
        let day: Option<NaiveDate> = row.get("day");
        if let Some((rate, rate_day)) = rate_between(db, &currency, reporting, day).await? {
            sqlx::query(
                "UPDATE opportunities
                    SET exchange_rate = $3, reporting_currency = $4,
                        rate_date = COALESCE(rate_date, $5::date::timestamptz)
                  WHERE id = $1 AND tenant_id = $2",
            )
            .bind(id)
            .bind(tenant_id)
            .bind(rate)
            .bind(reporting)
            .bind(rate_day)
            .execute(db)
            .await?;
            updated += 1;
        }
    }

    // Same-currency deals are 1 by definition and just need their destination stamped.
    let same = sqlx::query(
        "UPDATE opportunities SET exchange_rate = COALESCE(exchange_rate, 1),
                reporting_currency = upper($2),
                rate_date = COALESCE(rate_date, now())
          WHERE tenant_id = $1 AND upper(COALESCE(currency,'')) = upper($2)
            AND (reporting_currency IS DISTINCT FROM upper($2) OR exchange_rate IS NULL)",
    )
    .bind(tenant_id)
    .bind(reporting)
    .execute(db)
    .await?;
    updated += same.rows_affected();

    // Money custom fields carry their own copy of the same contract.
    sqlx::query(
        "UPDATE deal_field_values v
            SET money_exchange_rate = CASE
                    WHEN upper(v.money_currency) = upper($2) THEN 1
                    ELSE ROUND(((SELECT usd_rate FROM fx_rates f WHERE upper(f.currency) = upper($2)
                                  ORDER BY f.as_of DESC LIMIT 1)
                              / NULLIF((SELECT usd_rate FROM fx_rates f
                                         WHERE upper(f.currency) = upper(v.money_currency)
                                         ORDER BY f.as_of DESC LIMIT 1), 0))::numeric, 8)
                END,
                money_reporting_currency = upper($2)
          FROM deal_custom_fields f
         WHERE f.id = v.field_id AND f.tenant_id = $1
           AND v.money_amount IS NOT NULL AND COALESCE(v.money_currency,'') <> ''
           AND (v.money_reporting_currency IS DISTINCT FROM upper($2))",
    )
    .bind(tenant_id)
    .bind(reporting)
    .execute(db)
    .await?;

    Ok(updated)
}

/// Pull today's snapshot from a free, key-less provider and store it.
///
/// Returns `(currencies_stored, the day the rates are for)`. The provider quotes everything
/// against USD, which is exactly the shape `fx_rates` stores — one row per currency, no pair
/// matrix, so a refresh can never make two pairs disagree.
pub async fn refresh_rates(db: &PgPool) -> Result<(usize, NaiveDate), AppError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(2))
        .user_agent("CoreSwift-currency/1.0")
        .build()
        .map_err(|e| AppError::Internal(format!("fx client: {e}")))?;

    let resp = client
        .get("https://open.er-api.com/v6/latest/USD")
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("rate provider unreachable: {e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Internal(format!(
            "rate provider answered HTTP {}",
            resp.status()
        )));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| AppError::Internal(format!("rate provider sent unreadable JSON: {e}")))?;

    let updated: i64 = body
        .get("time_last_update_unix")
        .and_then(|v| v.as_i64())
        .unwrap_or_else(|| Utc::now().timestamp());
    let as_of = DateTime::<Utc>::from_timestamp(updated, 0)
        .unwrap_or_else(Utc::now)
        .date_naive();

    let rates = body
        .get("rates")
        .and_then(|r| r.as_object())
        .ok_or_else(|| AppError::Internal("rate provider returned no rates".to_string()))?;

    let mut stored = 0usize;
    for (code, value) in rates {
        let up = code.to_uppercase();
        if !is_iso_currency(&up) {
            continue;
        }
        let Some(rate) = value.as_f64() else { continue };
        if !rate.is_finite() || rate <= 0.0 {
            continue;
        }
        sqlx::query(
            "INSERT INTO fx_rates (currency, usd_rate, as_of, source)
             VALUES ($1, $2::numeric, $3, 'open.er-api.com')
             ON CONFLICT (currency, as_of) DO UPDATE
                SET usd_rate = EXCLUDED.usd_rate, source = EXCLUDED.source, fetched_at = NOW()",
        )
        .bind(&up)
        .bind(rate)
        .bind(as_of)
        .execute(db)
        .await?;
        stored += 1;
    }
    if stored == 0 {
        return Err(AppError::Internal(
            "rate provider returned no usable ISO currencies".to_string(),
        ));
    }
    Ok((stored, as_of))
}

/// One deal's money, resolved: the currency it trades in, the rate that converts it, the day that
/// rate was true, and the currency it converts INTO.
#[derive(Debug, Clone)]
pub struct MoneyQuote {
    pub currency: String,
    pub reporting_currency: String,
    pub rate: f64,
    pub rate_date: DateTime<Utc>,
}

/// Resolve "what currency is this deal in, and at what rate" for a write path that is not the deal
/// writer itself (the legacy pipeline routes and the CSV import) so they cannot drift back to a
/// silent USD assumption. Order is the same everywhere: what the caller said → the workspace
/// default → refusal. A cross-currency deal needs a rate; when the table has none the caller is
/// told both remedies instead of being given a guess.
pub async fn quote(
    db: &PgPool,
    tenant_id: Uuid,
    requested: Option<&str>,
) -> Result<MoneyQuote, AppError> {
    let (default, reporting) = workspace_currencies(db, tenant_id).await?;
    let currency = match requested.map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => normalize_iso(c)?,
        None => default.ok_or_else(|| {
            AppError::Validation(
                "this workspace has no default currency — say which currency the amount is in, or \
                 choose a workspace currency under Deals → Settings"
                    .to_string(),
            )
        })?,
    };
    let reporting_currency = reporting.unwrap_or_else(|| currency.clone());
    let (rate, day) = rate_between(db, &currency, &reporting_currency, None)
        .await?
        .ok_or_else(|| {
            AppError::Validation(format!(
                "no exchange rate for {currency} into {reporting_currency}: refresh the rate table \
                 under Deals → Settings"
            ))
        })?;
    Ok(MoneyQuote {
        currency,
        reporting_currency,
        rate,
        rate_date: day.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc(),
    })
}

/// The latest rate for every currency this app knows, for the console's settings panel.
pub async fn latest_rates(db: &PgPool) -> Result<Vec<(String, f64, NaiveDate)>, AppError> {
    let rows = sqlx::query(
        "SELECT DISTINCT ON (upper(currency)) upper(currency) AS currency,
                usd_rate::float8 AS usd_rate, as_of
           FROM fx_rates ORDER BY upper(currency), as_of DESC",
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.get::<String, _>("currency"),
                r.get::<f64, _>("usd_rate"),
                r.get::<NaiveDate, _>("as_of"),
            )
        })
        .collect())
}
