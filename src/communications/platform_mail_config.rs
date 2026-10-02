//! The PLATFORM mail transport: an admin-editable row first, the environment second.
//!
//! The platform transport is the sending identity every workspace WITHOUT its own credentials falls
//! back to (`resolve_email`). Until this module existed it read `EMAIL_API_URL` / `EMAIL_API_KEY` /
//! `EMAIL_FROM` and nothing else, so rotating that credential meant a shell edit of
//! `/etc/swift/env/coreswift.env` plus a container recreate — David could not manage it from the
//! admin panel, and nothing in the panel showed whether platform mail was configured at all
//! (kanban t_6a330ed2).
//!
//! ORDER — field by field, so a plain deploy can never regress today's working mail:
//!   1. the field in the `admin_settings` row keyed `email` (the fleet shape: provider / api_url /
//!      api_key / from_name / from_address);
//!   2. the `EMAIL_*` variable for THAT SAME field;
//!   3. nothing — which is logged and reported, never guessed.
//!
//! A half-filled row therefore still sends: a rotated key with no endpoint of its own rides on
//! `EMAIL_API_URL`. That is also why an EMPTY row is not "unconfigured" — it means "the server
//! environment carries this", which is exactly what `DELETE /api/admin/email-config` restores.
//!
//! The credential is SEALED at rest with the app's own `secret_box` envelope (`enc:v1:`), the same
//! one `provider_keys` uses. That envelope is tenant-scoped and this credential belongs to no
//! tenant, so it is sealed under a scope id of its own (`platform_scope`) — a platform ciphertext
//! copied into a tenant's row does not open, and a tenant's ciphertext copied here does not either.

use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

use super::providers::{self, PlatformMail};

/// `admin_settings` key holding the platform transport.
pub const CONFIG_KEY: &str = "email";

/// Row description, so the row explains itself to whoever reads the table.
pub const DESCRIPTION: &str = "Platform mail transport (admin-editable): provider + credential";

/// The mask the GET serves and the panel echoes back untouched. A round-trip carrying this value
/// keeps the stored credential; only an explicitly emptied field clears it.
pub const MASK: &str = "••••••••";

/// The provider the platform transport uses when the row does not name one. `deliver_email` picks
/// its arm from the WORKSPACE's own `email_provider`, so the platform half this app can carry is
/// the Mailgun pair.
pub const DEFAULT_PROVIDER: &str = "mailgun";

/// The providers the PLATFORM transport can carry. A workspace's own SMTP/SendGrid choice is its
/// own credential (Integration Center / provider settings), not this one.
const PROVIDERS: [(&str, &str); 1] = [("mailgun", "Mailgun")];

/// The credential field of the config object. Sealed at rest, never served back.
const SECRET_FIELDS: [&str; 1] = ["api_key"];

/// The scope id a platform (not tenant-owned) credential is sealed under. A reserved constant, not
/// a tenant id: `secret_box` derives its key from `(CORESWIFT_SECRET, scope)`, so this value is
/// what keeps the platform credential's ciphertext unreadable from inside any tenant's envelope.
fn platform_scope() -> Uuid {
    Uuid::from_u128(0x0000_0000_0000_0000_0000_0000_c0de_0001)
}

/// A non-empty, trimmed string field of a config object.
pub fn field(cfg: &Value, key: &str) -> Option<String> {
    cfg.get(key)
        .and_then(|v| v.as_str())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Is this a provider the platform transport can carry? (Never a silent fallback: the panel's
/// select is built from `providers_json`, and a value from anywhere else is refused by name.)
pub fn is_supported_provider(name: &str) -> bool {
    PROVIDERS.iter().any(|(value, _)| *value == name)
}

/// The provider list the GET serves and the panel renders.
pub fn providers_json() -> Value {
    Value::Array(
        PROVIDERS
            .iter()
            .map(|(value, label)| json!({ "value": value, "label": label }))
            .collect(),
    )
}

/// The allowed provider values, for an error message that names them.
pub fn provider_values() -> Vec<&'static str> {
    PROVIDERS.iter().map(|(value, _)| *value).collect()
}

/// Open the credential fields of a config object IN PLACE. A value without the envelope is a
/// legacy PLAINTEXT row and is passed through untouched (`secret_box::open` is the app-wide reader
/// and never fails).
fn open_secrets(cfg: &mut Value) {
    let Some(obj) = cfg.as_object_mut() else {
        return;
    };
    for name in SECRET_FIELDS {
        let stored = obj
            .get(name)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if stored.is_empty() || !crate::secret_box::is_sealed(&stored) {
            continue;
        }
        obj.insert(
            name.to_string(),
            Value::String(crate::secret_box::open(platform_scope(), &stored)),
        );
    }
}

/// Seal the credential fields of a config object IN PLACE, before it is stored.
///
/// FAILS CLOSED: with no master key (`CORESWIFT_SECRET`) configured, `secret_box::seal` errors and
/// the write is refused — a platform credential is never stored in the clear, and an empty field
/// stays empty rather than becoming a ciphertext of nothing.
pub fn seal_secrets(cfg: &mut Value) -> Result<(), crate::errors::AppError> {
    let Some(obj) = cfg.as_object_mut() else {
        return Ok(());
    };
    for name in SECRET_FIELDS {
        let current = obj
            .get(name)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if current.is_empty() || crate::secret_box::is_sealed(&current) {
            continue;
        }
        let sealed = crate::secret_box::seal(platform_scope(), &current)?;
        obj.insert(name.to_string(), Value::String(sealed));
    }
    Ok(())
}

/// The stored row, OPENED (the credential back in plaintext, so a caller can hand it to a provider
/// and never an envelope). `None` when there is no row — or when the read failed, which is logged
/// and treated as "no row", so a database hiccup falls through to the environment instead of
/// taking mail down.
pub async fn stored(pool: &PgPool) -> Option<Value> {
    let mut row: Value = sqlx::query_scalar::<_, Value>(
        "SELECT value FROM admin_settings WHERE key = $1",
    )
    .bind(CONFIG_KEY)
    .fetch_optional(pool)
    .await
    .inspect_err(|e| {
        tracing::warn!(
            error = %e,
            key = CONFIG_KEY,
            "admin_settings read failed — the platform mail transport falls back to the environment"
        )
    })
    .ok()
    .flatten()?;
    open_secrets(&mut row);
    Some(row)
}

/// Which store carried the resolved transport. Reported on every surface that must SAY where mail
/// is going, so "the panel says configured" and "the send path has a credential" cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Every field came from `EMAIL_*`.
    Environment,
    /// Every field came from the admin-editable row.
    Database,
    /// The row is filled in part; the rest came from `EMAIL_*`.
    Both,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Environment => "environment",
            Origin::Database => "database",
            Origin::Both => "database+environment",
        }
    }
}

/// The resolved platform transport, with the credential ready to hand to a provider.
#[derive(Debug, Clone)]
pub struct PlatformTransport {
    pub provider: String,
    pub url: String,
    pub api_key: String,
    /// The From identity as it will be sent: `Name <address>` when a name is configured.
    pub from: String,
    pub origin: Origin,
}

impl PlatformTransport {
    /// The transport as the delivery path takes it.
    pub fn mail(&self) -> PlatformMail {
        PlatformMail {
            url: self.url.clone(),
            api_key: self.api_key.clone(),
            from: self.from.clone(),
        }
    }

    /// The display form — never the key, never a digest of it: which store is carrying mail, the
    /// domain the endpoint sends through, and the From a receiver will see.
    pub fn view(&self) -> Value {
        json!({
            "provider": self.provider,
            "source": self.origin.as_str(),
            "domain": providers::domain_of(&self.url),
            "from": self.from,
            "api_url": self.url,
            "note": match self.origin {
                Origin::Database => "sending through the platform credential saved in this panel",
                Origin::Both => "sending through the saved platform credential, completed from the server environment",
                Origin::Environment => "sending through the server environment (EMAIL_API_URL / EMAIL_API_KEY / EMAIL_FROM) — save a credential here to take it over",
            }
        })
    }
}

/// Is a From identity a full `Name <address>` already?
fn address_of(identity: &str) -> Option<String> {
    let start = identity.find('<')?;
    let end = identity.find('>')?;
    if end <= start + 1 {
        return None;
    }
    Some(identity[start + 1..end].trim().to_string())
}

/// `Name <address>` when a name is configured, the bare address otherwise — and never a
/// double-wrapped `Name <Name <address>>` when the environment already carries a full identity.
fn compose_from(name: Option<&str>, address: &str) -> String {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(n) if !address.contains('<') => format!("{n} <{address}>"),
        _ => address.to_string(),
    }
}

/// Resolve the platform transport. `None` when neither store has a complete one — the missing
/// variables are named at ERROR level (names only, never values), which is what makes a
/// misconfiguration visible instead of a pile of undeliverable rows.
pub async fn resolved(pool: &PgPool) -> Option<PlatformTransport> {
    let row = stored(pool).await.unwrap_or_else(|| json!({}));
    let (env_url, env_key, env_from) = providers::platform_mail_env();

    let row_url = field(&row, "api_url");
    let row_key = field(&row, "api_key");
    let row_address = field(&row, "from_address");
    let row_name = field(&row, "from_name");
    let row_provider = field(&row, "provider");

    let url = row_url.clone().or(env_url);
    let api_key = row_key.clone().or(env_key);
    // The From can arrive as a bare address (our own shape) or as a full `Name <address>` identity
    // (what an operator tends to put in EMAIL_FROM). Never wrap the second one twice.
    let from = match (row_address.clone(), env_from) {
        (Some(address), _) => Some(compose_from(row_name.as_deref(), &address)),
        (None, Some(identity)) => Some(identity),
        (None, None) => None,
    };

    let (url, api_key, from) = match (url, api_key, from) {
        (Some(url), Some(api_key), Some(from)) => (url, api_key, from),
        (url, key, from) => {
            tracing::error!(
                email_api_url = url.is_some(),
                email_api_key = key.is_some(),
                email_from = from.is_some(),
                "{}: the platform mail transport is incomplete (admin panel → System Email, or EMAIL_API_URL / EMAIL_API_KEY / EMAIL_FROM)",
                providers::NOT_CONFIGURED_PREFIX
            );
            return None;
        }
    };

    let from_row = [row_url, row_key, row_address]
        .iter()
        .filter(|f| f.is_some())
        .count();
    let origin = match from_row {
        0 => Origin::Environment,
        3 => Origin::Database,
        _ => Origin::Both,
    };

    // Informational on this app (the delivery arm comes from the workspace's own provider), but a
    // stale or invented value in the row must not leak into a status payload as if it were real.
    let provider = match row_provider {
        Some(p) if is_supported_provider(&p) => p,
        Some(p) => {
            tracing::warn!(
                provider = %p,
                provider_used = DEFAULT_PROVIDER,
                "the stored platform email provider is not one this app's transport can carry — reporting the supported one"
            );
            DEFAULT_PROVIDER.to_string()
        }
        None => DEFAULT_PROVIDER.to_string(),
    };

    Some(PlatformTransport {
        provider,
        url,
        api_key,
        from,
        origin,
    })
}

/// The masked view of a stored row: presence, length and the non-credential fields ONLY.
///
/// The key itself is never here, and neither is a digest of it — a fingerprint is a brute-force
/// oracle for a short secret and the panel needs nothing more than "set / not set" to do its job.
/// `api_key_len` is stripped off the credential that is stored HERE (null when the row holds none),
/// so an operator can tell a whole paste from a truncated one without ever seeing the value.
pub fn masked_config(row: &Value) -> Value {
    let key = field(row, "api_key");
    json!({
        "provider": field(row, "provider").unwrap_or_else(|| DEFAULT_PROVIDER.to_string()),
        "api_url": field(row, "api_url"),
        "from_address": field(row, "from_address"),
        "from_name": field(row, "from_name"),
        "api_key": if key.is_some() { MASK } else { "" },
        "api_key_set": key.is_some(),
        "api_key_len": key.map(|k| k.chars().count()),
    })
}

/// Presence (never values) of the environment fallback, so the panel can explain WHY mail still
/// flows when the row is empty — or why it does not when neither store is filled in.
pub fn env_view() -> Value {
    let (url, key, from) = providers::platform_mail_env();
    let from_address = from
        .as_deref()
        .map(|f| address_of(f).unwrap_or_else(|| f.to_string()));
    json!({
        "api_url_set": url.is_some(),
        "api_key_set": key.is_some(),
        "from_address": from_address,
    })
}
