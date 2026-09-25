//! Secret-at-rest helper for third-party credentials (CS-21).
//!
//! `provider_keys.api_key` used to be stored in PLAINTEXT while the app already had AES-256-GCM for
//! private-email credentials. This module is the single place that seals/opens a tenant's provider
//! secret, so every read path goes through one implementation instead of six copies of a query.
//!
//! Format: `enc:v1:<base64(nonce || ciphertext)>`, tenant-scoped (the AES key is derived from the
//! SERVER SECRET IN THE ENVIRONMENT + the tenant id), which means one tenant's ciphertext cannot be
//! opened with another tenant's key even if the row is copied between them.
//!
//! Two halves, both required: the key must not live in this repository, and no writer may bypass
//! this module. `seal` refuses to write when the master key is missing (fail closed) and
//! `migrations/075_provider_keys_sealed_guard.sql` makes the DATABASE refuse a plaintext value, so a
//! future handler that forgets to seal fails loudly instead of quietly storing a live credential.
//!
//! `open()` NEVER fails: it returns the plaintext for a sealed value, transparently passes through a
//! legacy PLAINTEXT value (rows written before this change, which the startup backfill then seals),
//! and returns an empty string if a ciphertext cannot be authenticated. That is what makes the
//! rollout safe — a row that has not been migrated yet still resolves.

use crate::errors::AppError;
use sqlx::PgPool;
use uuid::Uuid;

const PREFIX: &str = "enc:v1:";

/// Is this value already sealed?
pub fn is_sealed(stored: &str) -> bool {
    stored.starts_with(PREFIX)
}

/// Encrypt a provider secret for storage. An EMPTY value stays empty: callers use `api_key <> ''`
/// (and `!key.is_empty()`) to mean "no credential configured", and sealing the empty string would
/// silently turn every empty slot into a configured one.
///
/// FAILS CLOSED when no master key is configured: a credential is never written in the clear, and
/// never written under a key that lives in the source tree (`encryption::is_configured`).
pub fn seal(tenant_id: Uuid, plaintext: &str) -> Result<String, AppError> {
    if plaintext.is_empty() {
        return Ok(String::new());
    }
    if !crate::private_email::encryption::is_configured() {
        tracing::error!(
            "refusing to store a provider credential: CORESWIFT_SECRET is not configured in the \
             environment"
        );
        return Err(AppError::Internal(
            "Refusing to store a credential: the server's encryption key is not configured. \
             An operator must set CORESWIFT_SECRET in the environment."
                .into(),
        ));
    }
    let ct = crate::private_email::encryption::encrypt_api_key(tenant_id, plaintext)
        .map_err(|e| AppError::Internal(format!("failed to seal provider key: {e}")))?;
    Ok(format!("{PREFIX}{ct}"))
}

/// Decrypt with the CONFIGURED key only.
fn open_configured(tenant_id: Uuid, value: &str) -> Option<String> {
    crate::private_email::encryption::decrypt_api_key(tenant_id, value).ok()
}

/// Does the CONFIGURED key open this STORED value — prefix and all? (The prefix is part of the
/// envelope, not of the ciphertext, so it has to come off before the AEAD is handed the value.)
fn opens_with_configured(tenant_id: Uuid, stored: &str) -> bool {
    let body = stored.strip_prefix(PREFIX).unwrap_or(stored);
    open_configured(tenant_id, body).is_some()
}

/// Decrypt with the built-in default key: rows sealed before the master key moved into the
/// environment. Read-only — the boot re-key rewrites them onto the configured key.
fn open_legacy(tenant_id: Uuid, value: &str) -> Option<String> {
    crate::private_email::encryption::decrypt_api_key_legacy(tenant_id, value).ok()
}

/// Decrypt a stored provider secret. Never fails — see the module note.
pub fn open(tenant_id: Uuid, stored: &str) -> String {
    if stored.is_empty() {
        return String::new();
    }
    if let Some(ct) = stored.strip_prefix(PREFIX) {
        if let Some(p) = open_configured(tenant_id, ct) {
            return p;
        }
        if let Some(p) = open_legacy(tenant_id, ct) {
            tracing::warn!(
                "provider key was sealed before the master key moved into the environment — \
                 the boot re-key migrates it"
            );
            return p;
        }
        // Wrong tenant, truncated value, or a lost master key. Loud, but never fatal: the caller
        // sees "no usable credential" instead of a 500.
        tracing::warn!("provider key ciphertext does not open for this tenant");
        return String::new();
    }
    // No prefix: a legacy PLAINTEXT row (returned as-is, so behaviour is unchanged until the
    // backfill seals it) or a value sealed before the prefix existed. AES-GCM authenticates, so a
    // plaintext value cannot accidentally "decrypt" into something else.
    if let Some(p) = open_configured(tenant_id, stored) {
        return p;
    }
    if let Some(p) = open_legacy(tenant_id, stored) {
        return p;
    }
    stored.to_string()
}

/// Does this stored value hold a usable credential? Used by the "is Telnyx configured" checks.
pub fn open_is_empty(tenant_id: Uuid, stored: &str) -> bool {
    open(tenant_id, stored).trim().is_empty()
}

/// Seal every provider key that is still plaintext, and RE-KEY every key that was sealed with the
/// built-in default before the master key moved into the environment (`CORESWIFT_SECRET`).
///
/// Idempotent, safe to run on every boot, and it never touches a row it cannot read. Runs before the
/// storage guard can be validated: after this, no row is plaintext and none depends on a key that
/// lives in the source tree.
pub async fn backfill_provider_keys(db: &PgPool) -> Result<usize, AppError> {
    if crate::private_email::encryption::is_configured() {
        tracing::info!("provider key encryption: enabled (master key from the environment)");
    } else {
        tracing::error!(
            "provider key encryption: DISABLED — CORESWIFT_SECRET is not set. Credential writes will \
             be refused (fail closed) rather than stored under the built-in default key."
        );
    }

    let rows: Vec<(Uuid, Uuid, String)> =
        sqlx::query_as("SELECT id, tenant_id, api_key FROM provider_keys WHERE api_key <> ''")
            .fetch_all(db)
            .await?;

    let mut sealed = 0usize;
    let mut rekeyed = 0usize;
    let mut unreadable = 0usize;
    for (id, tenant_id, stored) in rows {
        // Already sealed with the CURRENT key: nothing to do. A row sealed with the legacy key looks
        // sealed too, which is why the check is "does the configured key open it", not "has it got
        // the prefix".
        if opens_with_configured(tenant_id, &stored) {
            continue;
        }
        // Either a legacy PLAINTEXT row, or ciphertext from the default key. `open` tolerates both;
        // sealing the CIPHERTEXT would double-encrypt it, which is why the plaintext is recovered
        // first and an unreadable value is left untouched.
        let plaintext = open(tenant_id, &stored);
        if plaintext.is_empty() {
            unreadable += 1;
            tracing::warn!(
                row = %id,
                "provider key is neither readable nor sealed for this tenant — left untouched"
            );
            continue;
        }
        let out = match seal(tenant_id, &plaintext) {
            Ok(o) => o,
            Err(e) => {
                tracing::error!(row = %id, error = %e, "cannot seal provider key (fail closed)");
                continue;
            }
        };
        sqlx::query("UPDATE provider_keys SET api_key = $1, updated_at = now() WHERE id = $2")
            .bind(&out)
            .bind(id)
            .execute(db)
            .await?;
        // Count what was actually WRITTEN, and separately report whether it reads back — counting
        // only on the read-back made the log read "sealed=0" while a row had just been sealed, which
        // is exactly the kind of misleading line that wastes someone's afternoon.
        if is_sealed(&stored) {
            rekeyed += 1;
        } else {
            sealed += 1;
        }
        if !opens_with_configured(tenant_id, &out) {
            tracing::error!(row = %id, "a freshly sealed provider key does not read back — INVESTIGATE");
        }
    }
    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM provider_keys WHERE api_key <> '' AND api_key NOT LIKE 'enc:v1:%'",
    )
    .fetch_one(db)
    .await?;
    tracing::info!(
        sealed_now = sealed,
        rekeyed,
        unreadable,
        plaintext_remaining = remaining,
        "provider key encryption check"
    );
    Ok(sealed + rekeyed)
}

/// The storage guard is added by `migrations/075_provider_keys_sealed_guard.sql` as `NOT VALID`, so
/// that it can be armed on a table that still holds legacy rows. Once the backfill above has sealed
/// every row there is nothing left to exempt, and validating the constraint makes it fully enforced.
///
/// Best-effort by design: a missing constraint is a warning at boot, never a failure. New writes are
/// enforced either way.
pub async fn validate_provider_key_guard(db: &PgPool) -> Result<bool, AppError> {
    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM provider_keys WHERE api_key <> '' AND api_key NOT LIKE 'enc:v1:%'",
    )
    .fetch_one(db)
    .await?;
    if remaining > 0 {
        tracing::warn!(
            plaintext_remaining = remaining,
            "provider key storage guard stays NOT VALID until every row is sealed"
        );
        return Ok(false);
    }
    sqlx::query("ALTER TABLE provider_keys VALIDATE CONSTRAINT provider_keys_api_key_sealed")
        .execute(db)
        .await?;
    let validated: bool = sqlx::query_scalar(
        "SELECT convalidated FROM pg_constraint WHERE conname = 'provider_keys_api_key_sealed'",
    )
    .fetch_one(db)
    .await?;
    Ok(validated)
}

/// `migrations/078_webhook_endpoints_sealed_guard.sql` arms the webhook signing-secret guard the
/// same way: `NOT VALID`, so it could be added to a table that might still hold legacy rows. The
/// table is empty today, so there is nothing to exempt and validating it leaves the column fully
/// enforced from the first customer write — the posture `provider_keys` reached above.
///
/// Best-effort like its sibling: a missing constraint (migration not applied yet) is a warning at
/// boot, never a failure. New writes are enforced either way.
pub async fn validate_webhook_guard(db: &PgPool) -> Result<bool, AppError> {
    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM webhook_endpoints \
         WHERE secret IS NOT NULL AND secret <> '' AND secret NOT LIKE 'enc:v1:%'",
    )
    .fetch_one(db)
    .await?;
    if remaining > 0 {
        tracing::warn!(
            plaintext_remaining = remaining,
            "webhook endpoint storage guard stays NOT VALID until every secret is sealed"
        );
        return Ok(false);
    }
    sqlx::query(
        "ALTER TABLE webhook_endpoints VALIDATE CONSTRAINT webhook_endpoints_secret_sealed",
    )
    .execute(db)
    .await?;
    let validated: bool = sqlx::query_scalar(
        "SELECT convalidated FROM pg_constraint WHERE conname = 'webhook_endpoints_secret_sealed'",
    )
    .fetch_one(db)
    .await?;
    Ok(validated)
}

/// Seal any Google refresh token still stored in plaintext on a booking calendar (t_706da9df).
///
/// Same contract as `backfill_provider_keys`: idempotent, safe on every boot, and it never touches a
/// row it cannot read. A token that is already sealed with the CONFIGURED key is skipped (the check is
/// "does the configured key open it", not "has it got the prefix"); a legacy PLAINTEXT row is sealed;
/// a value sealed under a key this deployment does not have is left alone rather than double-encrypted.
///
/// The column held no rows when `migrations/079` armed the guard, so this is insurance for a row that
/// arrives from an older deployment or a restore — the case the guard's `NOT VALID` exemption covers.
///
/// The select item is COALESCEd to `''` (t_b25a9002): the column is NULLABLE and the WHERE already
/// excludes NULL/empty, so this is a no-op — it makes the non-nullability visible to the decode type
/// and to `fleet-dbtype-audit.py`, which judges the position against the column's nullability alone.
pub async fn backfill_booking_calendar_tokens(db: &PgPool) -> Result<usize, AppError> {
    let rows: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT id, tenant_id, COALESCE(google_refresh_token, '') AS google_refresh_token FROM booking_calendars \
         WHERE google_refresh_token IS NOT NULL AND google_refresh_token <> ''",
    )
    .fetch_all(db)
    .await?;

    let mut sealed = 0usize;
    let mut rekeyed = 0usize;
    let mut unreadable = 0usize;
    for (id, tenant_id, stored) in rows {
        if opens_with_configured(tenant_id, &stored) {
            continue;
        }
        let plaintext = open(tenant_id, &stored);
        if plaintext.is_empty() {
            unreadable += 1;
            tracing::warn!(
                row = %id,
                "booking calendar Google grant is neither readable nor sealed for this tenant \
                 — left untouched"
            );
            continue;
        }
        let out = match seal(tenant_id, &plaintext) {
            Ok(o) => o,
            Err(e) => {
                tracing::error!(
                    row = %id,
                    error = %e,
                    "cannot seal the booking calendar Google grant (fail closed)"
                );
                continue;
            }
        };
        sqlx::query(
            "UPDATE booking_calendars SET google_refresh_token = $1, updated_at = now() WHERE id = $2",
        )
        .bind(&out)
        .bind(id)
        .execute(db)
        .await?;
        if is_sealed(&stored) {
            rekeyed += 1;
        } else {
            sealed += 1;
        }
        if !opens_with_configured(tenant_id, &out) {
            tracing::error!(
                row = %id,
                "a freshly sealed booking calendar grant does not read back — INVESTIGATE"
            );
        }
    }
    tracing::info!(
        sealed,
        rekeyed,
        unreadable,
        "booking calendar Google grants at rest are encrypted"
    );
    Ok(sealed + rekeyed)
}

/// `migrations/079_booking_calendars_sealed_guard.sql` arms this guard `NOT VALID`, exactly like
/// 075 (provider_keys) and 078 (webhook_endpoints). 079 arrived without its second half, so the
/// constraint sat at `convalidated = f` and nothing asserted it at boot — an armed guard that no
/// start-up check ever mentioned, which is the posture that let the missing half of 075 go unnoticed
/// until t_6718dc86 copied it. Once the backfill above has sealed every row there is nothing left to
/// exempt, so validating it makes the column fully enforced from the first customer write.
///
/// Best-effort by design: a missing constraint (migration not applied yet) is a warning at boot,
/// never a failure. New writes are enforced either way.
pub async fn validate_booking_calendar_guard(db: &PgPool) -> Result<bool, AppError> {
    // Self-heal first: `sqlx::migrate!` embeds its file list at COMPILE time and the runner records
    // each file once, so a deployment whose 079 row was recorded but whose constraint is gone
    // (dropped by hand, or restored from a dump that omitted it) would never be guarded again —
    // `VALIDATE CONSTRAINT` against a missing name errors and the boot would only warn. Arm it
    // NOT VALID here; the validate below flips it to fully enforced once nothing is unsealed.
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_constraint \
         WHERE conname = 'booking_calendars_google_token_sealed')",
    )
    .fetch_one(db)
    .await?;
    if !exists {
        sqlx::query(
            "ALTER TABLE booking_calendars ADD CONSTRAINT booking_calendars_google_token_sealed \
             CHECK (google_refresh_token IS NULL OR google_refresh_token = '' OR \
             google_refresh_token LIKE 'enc:v1:%') NOT VALID",
        )
        .execute(db)
        .await?;
        tracing::warn!("booking calendar storage guard was missing — armed NOT VALID");
    }
    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM booking_calendars \
         WHERE google_refresh_token IS NOT NULL AND google_refresh_token <> '' \
         AND google_refresh_token NOT LIKE 'enc:v1:%'",
    )
    .fetch_one(db)
    .await?;
    if remaining > 0 {
        tracing::warn!(
            plaintext_remaining = remaining,
            "booking calendar storage guard stays NOT VALID until every grant is sealed"
        );
        return Ok(false);
    }
    sqlx::query(
        "ALTER TABLE booking_calendars VALIDATE CONSTRAINT booking_calendars_google_token_sealed",
    )
    .execute(db)
    .await?;
    let validated: bool = sqlx::query_scalar(
        "SELECT convalidated FROM pg_constraint \
         WHERE conname = 'booking_calendars_google_token_sealed'",
    )
    .fetch_one(db)
    .await?;
    Ok(validated)
}

/// Seal any portfolio integration-target credential still stored in plaintext (t_477d46c2).
///
/// `integration_targets.api_key` is the outbound credential a tenant pastes for a webhook/n8n/Zapier
/// target. It is SEALED on the create path since this card; this is the other half — a row written
/// before the fix (or restored from a dump) gets sealed on the next boot instead of sitting in the
/// clear beside sealed ones. Same contract as `backfill_provider_keys` and
/// `backfill_booking_calendar_tokens`: idempotent, safe on every boot, and it never touches a value
/// the configured key cannot open (no double encryption, no lost credential).
///
/// READ CONTRACT for future consumers: this column is ciphertext now. Anything that needs the
/// credential must call `crate::secret_box::open(tenant_id, stored)` — never interpolate
/// `integration_targets.api_key` into SQL or into an outbound header. Nothing consumes the plaintext
/// today (the only other reads are the redacted list in `portfolio::handlers` and a `COUNT(*)` in
/// `features.rs`), so there is deliberately no decrypt-on-read site to add.
///
/// The select item is COALESCEd to `''` for the same reason as `backfill_booking_calendar_tokens`
/// above (t_b25a9002): NULLABLE column, WHERE already excludes NULL/empty, decode stays non-Option.
pub async fn backfill_integration_target_keys(db: &PgPool) -> Result<usize, AppError> {
    let rows: Vec<(Uuid, Uuid, String)> = sqlx::query_as(
        "SELECT id, tenant_id, COALESCE(api_key, '') AS api_key FROM integration_targets \
         WHERE api_key IS NOT NULL AND api_key <> ''",
    )
    .fetch_all(db)
    .await?;

    let mut sealed = 0usize;
    let mut rekeyed = 0usize;
    let mut unreadable = 0usize;
    for (id, tenant_id, stored) in rows {
        if opens_with_configured(tenant_id, &stored) {
            continue;
        }
        let plaintext = open(tenant_id, &stored);
        if plaintext.is_empty() {
            unreadable += 1;
            tracing::warn!(
                row = %id,
                "integration target credential is neither readable nor sealed for this tenant \
                 — left untouched"
            );
            continue;
        }
        let out = match seal(tenant_id, &plaintext) {
            Ok(o) => o,
            Err(e) => {
                tracing::error!(
                    row = %id,
                    error = %e,
                    "cannot seal the integration target credential (fail closed)"
                );
                continue;
            }
        };
        sqlx::query(
            "UPDATE integration_targets SET api_key = $1, updated_at = now() WHERE id = $2",
        )
        .bind(&out)
        .bind(id)
        .execute(db)
        .await?;
        if is_sealed(&stored) {
            rekeyed += 1;
        } else {
            sealed += 1;
        }
        if !opens_with_configured(tenant_id, &out) {
            tracing::error!(
                row = %id,
                "a freshly sealed integration target credential does not read back — INVESTIGATE"
            );
        }
    }
    tracing::info!(
        sealed,
        rekeyed,
        unreadable,
        "integration target credentials at rest are encrypted"
    );
    Ok(sealed + rekeyed)
}

/// `migrations/077_integration_targets_sealed_guard.sql` arms this guard `NOT VALID`, exactly like
/// 075/078/079. It arrived without its second half, so `convalidated` sat at `f` and no boot line
/// asserted the column. Now that the backfill above has sealed every row there is nothing left to
/// exempt, so validating it makes the column fully enforced from the first customer write.
///
/// Self-healing on purpose: `sqlx::migrate!` embeds its file list at COMPILE time and the runner
/// records each file once, so a constraint dropped by hand (or a restore that omitted it) would
/// never come back and `VALIDATE CONSTRAINT` against a missing name only errors. Add-if-missing
/// first, then validate. Best-effort: a failure here warns at boot, never takes the app down.
pub async fn validate_integration_target_guard(db: &PgPool) -> Result<bool, AppError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'integration_targets_api_key_sealed')",
    )
    .fetch_one(db)
    .await?;
    if !exists {
        sqlx::query(
            "ALTER TABLE integration_targets ADD CONSTRAINT integration_targets_api_key_sealed \
             CHECK (api_key IS NULL OR api_key = '' OR api_key LIKE 'enc:v1:%') NOT VALID",
        )
        .execute(db)
        .await?;
        tracing::warn!("integration target storage guard was missing — armed NOT VALID");
    }
    let remaining: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM integration_targets \
         WHERE api_key IS NOT NULL AND api_key <> '' AND api_key NOT LIKE 'enc:v1:%'",
    )
    .fetch_one(db)
    .await?;
    if remaining > 0 {
        tracing::warn!(
            plaintext_remaining = remaining,
            "integration target storage guard stays NOT VALID until every credential is sealed"
        );
        return Ok(false);
    }
    sqlx::query(
        "ALTER TABLE integration_targets VALIDATE CONSTRAINT integration_targets_api_key_sealed",
    )
    .execute(db)
    .await?;
    let validated: bool = sqlx::query_scalar(
        "SELECT convalidated FROM pg_constraint WHERE conname = 'integration_targets_api_key_sealed'",
    )
    .fetch_one(db)
    .await?;
    Ok(validated)
}

// ── CS-21b: seal-on-WRITE audit ────────────────────────────────────────────────────────────────
//
// Every read path is safe (`open` tolerates legacy plaintext), but nothing FAILED LOUDLY when a
// column held a plaintext secret: `provider_keys` did for months. A new column, a new handler, or
// one manual `UPDATE` can reintroduce that silently. This audit is the assertion — it runs on every
// boot and, with `CORESWIFT_SECRET_AUDIT=1`, as a one-shot check that exits non-zero.

/// `(table, secret column)` for tables that carry a `tenant_id`. A value here is acceptable when it
/// is `enc:v1:`-sealed or a ciphertext this tenant's key can actually open.
const TENANT_SECRET_COLUMNS: &[(&str, &str)] = &[
    ("provider_keys", "api_key"),
    ("private_email_api_keys", "api_key_encrypted"),
    ("private_email_domains", "mailgun_api_key"),
    ("private_email_domains", "smtp_password_encrypted"),
    ("private_email_domains", "webhook_signing_key_encrypted"),
    // t_477d46c2: the portfolio integration target's outbound webhook credential. Sealed on
    // create since this run; the row here makes a regression fail loudly at every boot.
    ("integration_targets", "api_key"),
    // t_6718dc86: the outbound webhook signing secret. Sealed on create/update since this run;
    // the API now returns a mask instead of the raw value.
    ("webhook_endpoints", "secret"),
    // t_706da9df: the Google OAuth refresh token on a booking calendar — a standing grant on the
    // tenant's Google account. Sealed on write, opened at the one read-for-use site.
    ("booking_calendars", "google_refresh_token"),
];

/// `(table, secret column)` for GLOBAL config rows. There is no tenant id to derive a key from, so a
/// non-empty value here can never be sealed — the only safe state is empty, and a populated value is
/// reported as a finding so whoever fills it in knows to move it to a tenant slot.
const GLOBAL_SECRET_COLUMNS: &[(&str, &str)] = &[
    ("telnyx_config", "api_key"),
    ("telnyx_config", "webhook_secret"),
];

/// One stored value that must not be there.
#[derive(Debug, serde::Serialize)]
pub struct SecretFinding {
    pub table: &'static str,
    pub column: &'static str,
    pub row_id: Uuid,
    pub tenant_id: Option<Uuid>,
    pub reason: &'static str,
}

/// The audit's result. `plaintext` is the dangerous case (a credential readable by anyone with the
/// dump); `unreadable` is a credential the app itself cannot open — usually a rotated
/// `CORESWIFT_SECRET` or a row written by another deployment. Both are reported; only the first is a
/// failure, because a rotation is an incident to look at, not a write path that is leaking.
#[derive(Debug, Default, serde::Serialize)]
pub struct SecretAudit {
    pub plaintext: Vec<SecretFinding>,
    pub unreadable: Vec<SecretFinding>,
}

/// Would this value have to be ciphertext to look the way it does?
///
/// Used only to separate "a credential was written in the clear" from "ciphertext this key cannot
/// open". A real credential is almost never valid standard base64 AND long enough to be
/// nonce+ciphertext+tag (>= 28 bytes); anything holding `-`, `_`, `:` or a space is plainly a
/// credential. The residual blind spot — a long, purely `[A-Za-z0-9+/]` plaintext secret — still
/// lands in `unreadable`, i.e. still reported, just not labelled as plaintext.
fn ciphertext_shaped(stored: &str) -> bool {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine as _;
    match B64.decode(stored) {
        Ok(bytes) => bytes.len() >= 28,
        Err(_) => false,
    }
}

/// Every stored secret that is in the clear or that the app cannot open. A healthy database has
/// `plaintext == []`; `unreadable` should be empty too, and is worth investigating when it is not.
pub async fn audit_plaintext_secrets(db: &PgPool) -> Result<SecretAudit, AppError> {
    let mut out = SecretAudit::default();

    for (table, column) in TENANT_SECRET_COLUMNS {
        let sql = format!(
            "SELECT id, tenant_id, {column} AS v FROM {table} \
             WHERE {column} IS NOT NULL AND {column} <> ''"
        );
        let rows: Vec<(Uuid, Uuid, String)> = sqlx::query_as(&sql).fetch_all(db).await?;
        let mut sealed = 0usize;
        let mut opened = 0usize;
        // Counts are per-table deltas, not the running totals: logging `out.plaintext.len()` here
        // printed the CUMULATIVE number on every table's line, so a single bad row in one table made
        // eight tables each report `unreadable=2` (t_45772522).
        let plaintext_before = out.plaintext.len();
        let unreadable_before = out.unreadable.len();
        for (row_id, tenant_id, stored) in rows {
            if is_sealed(&stored) {
                // A PREFIX IS NOT PROOF. `sealed += 1` used to be decided by the string alone, so a
                // row wearing `enc:v1:` that this deployment cannot open was reported as healthy
                // forever — the same blindness that hid two dead rows in private_email_api_keys
                // (t_45772522). A sealed row counts only once the key actually opens it; otherwise
                // it is a finding, exactly like an unopenable unprefixed value.
                if opens_with_configured(tenant_id, &stored)
                    || open_legacy(tenant_id, &stored).is_some()
                {
                    sealed += 1;
                    continue;
                }
                out.unreadable.push(SecretFinding {
                    table,
                    column,
                    row_id,
                    tenant_id: Some(tenant_id),
                    reason:
                        "enc:v1:-sealed but neither the configured nor the built-in key opens it \
                             (rotated or lost master key, or written by another deployment)",
                });
                continue;
            }
            // Not prefixed: it must still be ciphertext from the older `encryption::encrypt_api_key`
            // shape (that is what the private-email tables store). AES-GCM authenticates, so a
            // plaintext value cannot pass this by accident. Both the configured and the legacy key
            // are tried, so this reports "the app can read it" exactly as `open` does.
            if open_configured(tenant_id, &stored).is_some()
                || open_legacy(tenant_id, &stored).is_some()
            {
                opened += 1;
                continue;
            }
            let shaped = ciphertext_shaped(&stored);
            let finding = SecretFinding {
                table,
                column,
                row_id,
                tenant_id: Some(tenant_id),
                reason: if shaped {
                    "ciphertext this tenant's key cannot open (rotated secret, or written by another deployment)"
                } else {
                    "not enc:v1:-sealed and not decryptable ciphertext — this looks like a plaintext credential"
                },
            };
            if shaped {
                out.unreadable.push(finding);
            } else {
                out.plaintext.push(finding);
            }
        }
        tracing::info!(
            table,
            column,
            sealed,
            opened,
            plaintext = out.plaintext.len() - plaintext_before,
            unreadable = out.unreadable.len() - unreadable_before,
            "secret column audit"
        );
    }

    for (table, column) in GLOBAL_SECRET_COLUMNS {
        let sql = format!(
            "SELECT id, {column} AS v FROM {table} WHERE {column} IS NOT NULL AND {column} <> ''"
        );
        let rows: Vec<(Uuid, String)> = sqlx::query_as(&sql).fetch_all(db).await?;
        for (row_id, _stored) in rows {
            out.plaintext.push(SecretFinding {
                table,
                column,
                row_id,
                tenant_id: None,
                reason:
                    "global config row: no tenant key to seal with, so the value must stay empty",
            });
        }
    }

    Ok(out)
}
