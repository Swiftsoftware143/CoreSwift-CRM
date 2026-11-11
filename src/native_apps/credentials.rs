//! Seal-on-write / open-on-use for the `app_connections.credentials` JSONB column
//! (kanban t_a1ada5ef).
//!
//! A sister app's API key is a DURABLE credential that opens a whole other product: it is not a
//! session, it does not expire with CoreSwift, and the tenant cannot revoke it from here. It must
//! therefore not sit in the clear in a JSONB column, where a database dump, a backup, a
//! read-grant support/impersonation session or any future analytics job can lift it.
//!
//! CoreSwift already has ONE envelope for credentials at rest — [`crate::secret_box`], the
//! `enc:v1:` AES-256-GCM envelope keyed by `(CORESWIFT_SECRET, tenant_id)` — so this module is the
//! single choke point for THIS column and invents no second scheme. Every writer of
//! `app_connections.credentials` calls [`seal_for_storage`]; every reader that hands the object to
//! `connectors::*` calls [`open_for_use`].
//!
//! Contract:
//!   * WRITE — [`seal_for_storage`] seals each secret field before INSERT/UPDATE. It FAILS CLOSED
//!     when the master key is absent: a credential is never written in the clear, and never under
//!     a key that lives in the source tree.
//!   * READ-FOR-USE — [`open_for_use`] opens the envelope. A legacy plaintext row (no `enc:v1:`
//!     prefix) keeps working and is sealed by [`backfill_app_connection_credentials`] at boot.
//!   * A value this deployment cannot open FAILS CLOSED (a 422 sentence): it is never handed to a
//!     connector as plaintext and never re-stored as plaintext.
//!   * Non-secret config (`base_url`, …) is NOT sealed — the Integration Center renders it.
//!
//! The column is guarded at the database level too (`migrations/131_…`), so a future writer that
//! forgets to seal is refused by Postgres instead of quietly storing a plaintext credential.

use crate::errors::AppError;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// The fields of `credentials` that hold a secret. Everything else is configuration.
pub const SECRET_FIELDS: &[&str] = &["api_key"];

/// The database guard this module owns (see `migrations/131_app_connections_credentials_sealed.sql`
/// and [`validate_app_connection_guard`]).
pub const GUARD_NAME: &str = "app_connections_credentials_sealed";

/// Arm the guard. Kept as compile-time constants: the fleet gate forbids building DDL at run time,
/// and a unit test asserts both literals name [`GUARD_NAME`].
const GUARD_ADD_SQL: &str = "ALTER TABLE app_connections ADD CONSTRAINT app_connections_credentials_sealed \
     CHECK (credentials->>'api_key' IS NULL OR credentials->>'api_key' = '' OR credentials->>'api_key' LIKE 'enc:v1:%') NOT VALID";
const GUARD_VALIDATE_SQL: &str =
    "ALTER TABLE app_connections VALIDATE CONSTRAINT app_connections_credentials_sealed";
const GUARD_SELECT_REMAINING_SQL: &str = "SELECT count(*) FROM app_connections \
     WHERE credentials->>'api_key' IS NOT NULL AND credentials->>'api_key' <> '' \
       AND credentials->>'api_key' NOT LIKE 'enc:v1:%'";

/// Does this credentials object carry a stored secret (sealed or legacy plaintext)?
///
/// Used by `app_status` so the panel can say "a credential is stored" WITHOUT ever being handed the
/// value — neither the plaintext nor the ciphertext.
pub fn has_secret(credentials: &Value) -> bool {
    let Some(obj) = credentials.as_object() else {
        return false;
    };
    SECRET_FIELDS.iter().any(|field| {
        obj.get(*field)
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
    })
}

/// Seal every secret field of a credentials object for storage.
///
/// Fails closed: with no master key (`CORESWIFT_SECRET`) this returns an error and the caller must
/// not write the row. An already-sealed value is left untouched (sealing a ciphertext again would
/// put an OUTER envelope around the inner one, and the connector would be handed an envelope).
pub fn seal_for_storage(tenant_id: Uuid, credentials: &Value) -> Result<Value, AppError> {
    let obj = credentials
        .as_object()
        .ok_or_else(|| AppError::Validation("credentials must be a JSON object".into()))?;
    let mut out = obj.clone();
    for field in SECRET_FIELDS {
        let Some(value) = obj.get(*field) else {
            continue;
        };
        let plain = value.as_str().ok_or_else(|| {
            AppError::Validation(format!("credentials field '{field}' must be a string"))
        })?;
        if plain.is_empty() || crate::secret_box::is_sealed(plain) {
            continue;
        }
        let sealed = crate::secret_box::seal(tenant_id, plain)?;
        out.insert((*field).to_string(), Value::String(sealed));
    }
    Ok(Value::Object(out))
}

/// Open a stored credentials object for USE by a connector.
///
/// A legacy plaintext row (no envelope) is returned unchanged — `/api/native/apps/*/connect` wrote
/// those before this module existed and the boot backfill seals them; until then they keep working.
///
/// A sealed value this deployment cannot open (rotated/lost master key, or a row copied from another
/// tenant) is a REFUSAL, never a silent empty string: the caller gets a 422 sentence telling the
/// tenant to reconnect, and the connector is never called with a credential the app could not read.
pub fn open_for_use(tenant_id: Uuid, credentials: &Value) -> Result<Value, AppError> {
    let obj = credentials.as_object().ok_or_else(|| {
        AppError::Validation(
            "No credentials are stored for this app — connect it first.".to_string(),
        )
    })?;
    let mut out = obj.clone();
    for field in SECRET_FIELDS {
        let Some(stored) = obj.get(*field).and_then(|v| v.as_str()) else {
            continue;
        };
        if stored.is_empty() {
            continue;
        }
        let plain = crate::secret_box::open(tenant_id, stored);
        if plain.is_empty() {
            tracing::warn!(
                field = *field,
                "the stored app-connection credential does not open for this tenant — refusing to \
                 use it (reconnect the app to store it again)"
            );
            return Err(AppError::Validation(
                "The stored credential for this app cannot be read by this deployment — reconnect \
                 the app to store it again."
                    .to_string(),
            ));
        }
        out.insert((*field).to_string(), Value::String(plain));
    }
    Ok(Value::Object(out))
}

/// Seal every `app_connections` row still holding a plaintext credential. Idempotent, safe on every
/// boot, and it never touches a row it cannot read (logged, left alone, never rewritten as
/// plaintext).
pub async fn backfill_app_connection_credentials(db: &PgPool) -> Result<usize, AppError> {
    let rows: Vec<(Uuid, Uuid, Value)> =
        sqlx::query_as("SELECT id, tenant_id, credentials FROM app_connections")
            .fetch_all(db)
            .await?;

    let mut sealed = 0usize;
    let mut unreadable = 0usize;
    for (id, tenant_id, stored) in rows {
        let sealed_value = match seal_for_storage(tenant_id, &stored) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!(
                    row = %id,
                    error = %e,
                    "cannot seal the app-connection credential (fail closed) — row left untouched"
                );
                unreadable += 1;
                continue;
            }
        };
        if sealed_value == stored {
            continue;
        }
        sqlx::query(
            "UPDATE app_connections SET credentials = $1, updated_at = NOW() WHERE id = $2",
        )
        .bind(&sealed_value)
        .bind(id)
        .execute(db)
        .await?;
        sealed += 1;
        if open_for_use(tenant_id, &sealed_value).is_err() {
            tracing::error!(
                row = %id,
                "a freshly sealed app-connection credential does not read back — INVESTIGATE"
            );
        }
    }
    if sealed > 0 || unreadable > 0 {
        tracing::info!(
            sealed,
            unreadable,
            "app connection credentials at rest are encrypted"
        );
    }
    Ok(sealed)
}

/// Make the storage guard fully enforced, self-healing on every boot.
///
/// `sqlx::migrate!` embeds its file list at COMPILE time and the runner records each file once, so a
/// constraint dropped by hand (or a restore that omitted it) would never come back — and
/// `VALIDATE CONSTRAINT` against a missing name only errors. Add-if-missing first, then validate
/// once nothing plaintext remains. Best-effort: a failure warns at boot, never takes the app down.
pub async fn validate_app_connection_guard(db: &PgPool) -> Result<bool, AppError> {
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'app_connections_credentials_sealed')",
    )
    .fetch_one(db)
    .await?;
    if !exists {
        sqlx::query(GUARD_ADD_SQL).execute(db).await?;
        tracing::warn!("app connection storage guard was missing — armed NOT VALID");
    }
    let remaining: i64 = sqlx::query_scalar(GUARD_SELECT_REMAINING_SQL)
        .fetch_one(db)
        .await?;
    if remaining > 0 {
        tracing::warn!(
            plaintext_remaining = remaining,
            "app connection storage guard stays NOT VALID until every credential is sealed"
        );
        return Ok(false);
    }
    sqlx::query(GUARD_VALIDATE_SQL).execute(db).await?;
    let validated: bool = sqlx::query_scalar(
        "SELECT convalidated FROM pg_constraint WHERE conname = 'app_connections_credentials_sealed'",
    )
    .fetch_one(db)
    .await?;
    Ok(validated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    /// The envelope reads `CORESWIFT_SECRET` from the process environment, so the tests that need a
    /// key (and the one that needs it GONE) take turns instead of racing each other.
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    const TEST_SECRET: &str = "coreswift-unit-test-master-secret-0123456789";

    fn with_key<T>(f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("CORESWIFT_SECRET", TEST_SECRET);
        f()
    }

    /// seal → read back → the connector receives the plaintext it was given, and no other key.
    #[test]
    fn seal_then_open_round_trips_and_keeps_config_readable() {
        with_key(|| {
            let tenant = Uuid::new_v4();
            let creds = json!({
                "api_key": "ada_9748SECRETVALUE0123456789",
                "base_url": "http://127.0.0.1:8087",
            });

            let sealed = seal_for_storage(tenant, &creds).expect("seal");
            let stored = sealed["api_key"].as_str().unwrap();
            assert!(
                stored.starts_with("enc:v1:"),
                "api_key is not sealed: {stored}"
            );
            assert_eq!(
                sealed["base_url"], "http://127.0.0.1:8087",
                "base_url is config, not a secret — it must stay readable"
            );
            assert!(
                !sealed.to_string().contains("ada_9748SECRETVALUE0123456789"),
                "the plaintext credential is still somewhere in the stored object"
            );

            let opened = open_for_use(tenant, &sealed).expect("open");
            assert_eq!(opened["api_key"], "ada_9748SECRETVALUE0123456789");
            assert_eq!(opened["base_url"], "http://127.0.0.1:8087");
            assert!(has_secret(&sealed));

            // Sealing an already-sealed object is a no-op (no outer envelope around the inner one).
            let again = seal_for_storage(tenant, &sealed).expect("re-seal");
            assert_eq!(again, sealed);
        });
    }

    /// Rows written before this module existed are plaintext; they keep working until the boot
    /// backfill seals them.
    #[test]
    fn legacy_plaintext_row_keeps_working() {
        with_key(|| {
            let tenant = Uuid::new_v4();
            let legacy =
                json!({"api_key": "workflow-legacy-key", "base_url": "http://127.0.0.1:8085"});
            let opened = open_for_use(tenant, &legacy).expect("legacy row must open");
            assert_eq!(opened["api_key"], "workflow-legacy-key");
            assert!(has_secret(&legacy));
        });
    }

    /// FAILS CLOSED: a row sealed under a key this deployment does not have (another tenant's
    /// ciphertext, or one written before the master key was rotated) is REFUSED — never handed to a
    /// connector as plaintext, and the refusal never echoes the stored value.
    #[test]
    fn unopenable_ciphertext_fails_closed() {
        with_key(|| {
            let tenant_a = Uuid::new_v4();
            let tenant_b = Uuid::new_v4();
            let sealed_for_a = seal_for_storage(tenant_a, &json!({"api_key": "md_94oI7-secret"}))
                .expect("seal for tenant A");
            let stored = sealed_for_a["api_key"].as_str().unwrap().to_string();
            assert!(stored.starts_with("enc:v1:"));

            // Tenant B cannot open it (the envelope is tenant-scoped) …
            let err = open_for_use(tenant_b, &sealed_for_a).expect_err("must fail closed");
            let msg = err.to_string();
            assert!(!msg.contains(&stored), "the refusal echoed the ciphertext");
            assert!(
                !msg.contains("md_94oI7-secret"),
                "the refusal echoed the plaintext"
            );

            // … and so does a truncated / garbage envelope.
            let garbage = json!({"api_key": "enc:v1:not-a-real-ciphertext"});
            assert!(open_for_use(tenant_a, &garbage).is_err());
        });
    }

    /// FAILS CLOSED on the WRITE side too: with no master key configured the credential is refused,
    /// never stored in the clear as a "fallback".
    #[test]
    fn missing_master_key_refuses_to_store() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var("CORESWIFT_SECRET").ok();
        std::env::set_var("CORESWIFT_SECRET", "too-short");

        let tenant = Uuid::new_v4();
        let creds = json!({"api_key": "ada_9748SECRETVALUE0123456789"});
        let err = seal_for_storage(tenant, &creds).expect_err("must fail closed without a key");
        assert!(
            !err.to_string().contains("ada_9748SECRETVALUE0123456789"),
            "the refusal echoed the credential"
        );

        match previous {
            Some(v) => std::env::set_var("CORESWIFT_SECRET", v),
            None => std::env::remove_var("CORESWIFT_SECRET"),
        }
    }

    /// Every DDL literal names the guard the module validates — no drift between the two.
    #[test]
    fn guard_literals_name_the_constraint() {
        for sql in [GUARD_ADD_SQL, GUARD_VALIDATE_SQL] {
            assert!(
                sql.contains(GUARD_NAME),
                "DDL literal misses {GUARD_NAME}: {sql}"
            );
        }
        // The remaining-plaintext census must use the same prefix the envelope writes.
        assert!(GUARD_SELECT_REMAINING_SQL.contains("enc:v1:"));
    }
}
