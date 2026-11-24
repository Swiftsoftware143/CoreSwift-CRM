//! Personal API Keys — per-user keys for third-party apps (Zapier-style).
//! A CoreSwift user generates a key, pastes it into the third-party app
//! (e.g. IncentiveSwift), and that app pushes data into THIS user's tenant.

use axum::{
    extract::{Extension, Path, State},
    http::HeaderMap,
    middleware,
    response::IntoResponse,
    Json,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

fn key_hash(key: &str) -> String {
    let mut h = Sha256::new();
    h.update(key.as_bytes());
    hex::encode(h.finalize())
}

fn generate_key() -> String {
    // Format: csk_<128-bit><128-bit> hex (CoreSwift Key), readable + high entropy
    format!("csk_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn key_prefix(full_key: &str) -> String {
    full_key.get(0..12).unwrap_or("csk_").to_string()
}

/// A key that was just minted. `key` is the only time the plaintext exists in memory before it is
/// sealed; callers hand it to the UI, never to a log.
pub struct MintedKey {
    pub id: Uuid,
    pub name: String,
    pub key: String,
    pub prefix: String,
}

/// The role an issued key carries. `workspace` is the customer's own key (read and write inside
/// their own workspace); `read_only` may call read tools only — the MCP door refuses it on every
/// write before the handler runs. Operator authority is NOT a key role: it comes from
/// `users.is_platform_admin` (see `src/mcp_door/mod.rs::Resolver`).
pub const ROLE_WORKSPACE: &str = "workspace";
pub const ROLE_READ_ONLY: &str = "read_only";

/// Mint one key for a tenant and INSERT it on the caller's connection.
///
/// One writer for every mint in this app: `create_key`, `rotate_key`, the signup transaction
/// (`auth::signup::create_account`) and the one-time backfill all call this, so the hash / prefix /
/// sealed-copy triple can never drift between them.
///
/// The row carries a sealed copy of the plaintext (`secret_box::seal`, AES-256-GCM under the
/// server secret + tenant id) purely so the owner can VIEW it again later. Authentication reads
/// `key_hash` only — `external_api::resolve_key` never touches `key_sealed`.
pub async fn mint_on(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    user_id: Option<Uuid>,
    name: &str,
) -> Result<MintedKey, AppError> {
    mint_with_role(conn, tenant_id, user_id, name, ROLE_WORKSPACE).await
}

/// The role-aware mint. ONE writer for every key in this app: `create_key`, `rotate_key`, the
/// signup transaction and `crm-swift mint-key` all end here, so the hash / prefix / sealed-copy
/// triple and the role can never drift between them.
pub async fn mint_with_role(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    user_id: Option<Uuid>,
    name: &str,
    api_role: &str,
) -> Result<MintedKey, AppError> {
    if api_role != ROLE_WORKSPACE && api_role != ROLE_READ_ONLY {
        return Err(AppError::BadRequest(format!(
            "'{api_role}' is not a key role — use '{ROLE_WORKSPACE}' or '{ROLE_READ_ONLY}'"
        )));
    }
    let full_key = generate_key();
    let hash = key_hash(&full_key);
    let prefix = key_prefix(&full_key);
    // Fail closed: `seal` refuses when the server secret is missing, so a key is never stored in
    // the clear (and never minted unusable-but-unrevealable).
    let sealed = crate::secret_box::seal(tenant_id, &full_key)?;
    let id = Uuid::new_v4();

    sqlx::query(
        "INSERT INTO personal_api_keys
             (id, tenant_id, user_id, name, key_hash, key_prefix, key_sealed, api_role)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(name)
    .bind(&hash)
    .bind(&prefix)
    .bind(&sealed)
    .bind(api_role)
    .execute(&mut *conn)
    .await
    .map_err(AppError::Database)?;

    Ok(MintedKey {
        id,
        name: name.to_string(),
        key: full_key,
        prefix,
    })
}

#[derive(Deserialize)]
pub struct CreateKeyRequest {
    pub name: Option<String>,
    /// `workspace` (default) or `read_only`. Anything else is refused, not defaulted.
    pub role: Option<String>,
}

/// POST /api/personal-api-keys — generate a new key
/// Key management is an ACCOUNT-ADMIN surface: an issued key is the whole workspace's credential,
/// so only an owner/admin may list, mint, rotate, revoke or reveal one. A `member` reaching these
/// routes could otherwise rotate the owner's key and walk away with the new plaintext.
fn require_key_admin(c: &Claims) -> Result<(), AppError> {
    crate::auth::middleware::require_account_admin(c)
}

pub async fn create_key(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Json(req): Json<CreateKeyRequest>,
) -> ApiResult<impl IntoResponse> {
    require_key_admin(&c)?;
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let user_id = Uuid::parse_str(&c.sub).ok();
    let name = req.name.unwrap_or_else(|| "default".to_string());
    let role = req
        .role
        .unwrap_or_else(|| ROLE_WORKSPACE.to_string())
        .trim()
        .to_string();
    if role != ROLE_WORKSPACE && role != ROLE_READ_ONLY {
        return Err(AppError::BadRequest(format!(
            "'{role}' is not a key role — use '{ROLE_WORKSPACE}' or '{ROLE_READ_ONLY}'"
        )));
    }

    // A workspace may hold SEVERAL active keys, each with its own name (migrations/207 dropped the
    // one-active partial unique index that 152 added), because a second key exists for a second
    // reason: a sibling app's Custom Objects integration (POST /api/mcp) must not have to rotate —
    // and therefore kill — the workspace's existing key. The cap keeps that from becoming a
    // credential pile-up: past ten, the caller is told to revoke one instead.
    let active: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM personal_api_keys WHERE tenant_id = $1 AND is_active = true",
    )
    .bind(tenant_id)
    .fetch_one(&s.db)
    .await
    .map_err(AppError::Database)?;
    if active >= 10 {
        return Err(AppError::Conflict(
            "This account already has 10 active API keys. Revoke one before minting another."
                .into(),
        ));
    }

    let mut tx = s.db.begin().await.map_err(AppError::Database)?;
    let minted = mint_with_role(&mut tx, tenant_id, user_id, &name, &role).await?;
    tx.commit().await.map_err(AppError::Database)?;

    Ok(Json(json!({
        "id": minted.id.to_string(),
        "name": minted.name,
        "role": role,
        "key": minted.key,
        "prefix": minted.prefix,
        "note": "This key can be viewed again in Settings > Integration Center after re-entering your password."
    })))
}

/// GET /api/personal-api-keys — list keys (masked)
pub async fn list_keys(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
) -> ApiResult<impl IntoResponse> {
    require_key_admin(&c)?;
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let rows = sqlx::query(
        "SELECT id, name, key_prefix, api_role, is_active, last_used_at, created_at,
                (key_sealed IS NOT NULL) AS recoverable
         FROM personal_api_keys WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(tenant_id)
    .fetch_all(&s.db)
    .await
    .map_err(AppError::Database)?;

    let keys: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<Uuid, _>("id").to_string(),
                "name": r.get::<String, _>("name"),
                "prefix": r.get::<String, _>("key_prefix"),
                "role": r.get::<String, _>("api_role"),
                "is_active": r.get::<bool, _>("is_active"),
                "last_used_at": r.get::<Option<chrono::DateTime<chrono::Utc>>, _>("last_used_at"),
                "created_at": r.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
                // `false` = minted before keys were stored sealed; the plaintext is gone for good.
                // The UI offers Regenerate for those instead of a View button that cannot work.
                "recoverable": r.get::<bool, _>("recoverable"),
            })
        })
        .collect();

    Ok(Json(json!({ "keys": keys })))
}

/// DELETE /api/personal-api-keys/:id — revoke a key
///
/// Revocation KEEPS the row and clears `is_active`, matching the rest of the fleet (and the CEO's
/// written contract: "is_active = false on revoke"): the key stops authenticating everywhere at once
/// — `external_api::resolve_key`, this app's own reads and the fleet MCP server all filter on
/// `is_active` — while the record of it stays for support and audit. Deleting the row instead would
/// leave no trace that the key ever existed.
pub async fn revoke_key(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    require_key_admin(&c)?;
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    sqlx::query("UPDATE personal_api_keys SET is_active = false WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tenant_id)
        .execute(&s.db)
        .await
        .map_err(AppError::Database)?;
    Ok(Json(json!({ "revoked": true })))
}

/// POST /api/personal-api-keys/:id/rotate — replace a key with a fresh secret.
///
/// The full key is only ever shown once, so without this the only way to "rotate" was
/// revoke-then-create, which silently breaks whatever capture app was already using the
/// key (it keeps POSTing against a dead key and the leads stop). Rotate keeps the name
/// and swaps the secret in one step: the new key is returned once, the old row is
/// removed, so the previous secret stops working immediately.
pub async fn rotate_key(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    require_key_admin(&c)?;
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let user_id = Uuid::parse_str(&c.sub).ok();

    // The ROLE travels with the rotation. Reading only `name` here (the shape this route shipped
    // with) silently re-minted every rotated key as a `workspace` key, so regenerating a read-only
    // key handed the customer a read-WRITE credential — a privilege escalation behind a button that
    // promised nothing but a new secret. Select both and mint with the role the row already carries.
    let row = sqlx::query(
        "SELECT name, api_role FROM personal_api_keys WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&s.db)
    .await
    .map_err(AppError::Database)?
    .ok_or_else(|| AppError::NotFound("API key not found".into()))?;
    let name: String = row.get("name");
    let role: String = row.get("api_role");

    let mut tx = s.db.begin().await.map_err(AppError::Database)?;
    // Retire the old key FIRST, in the same transaction as the mint. `migrations/152` used to add a
    // partial unique index on (tenant_id) WHERE is_active, so insert-then-delete would have raised
    // a unique violation; `migrations/207` dropped that index (a workspace may hold several active
    // keys), but retire-first is still the correct order and this is not the place to change it:
    // it keeps a rotation from EVER having two working secrets at once, and the rotate response
    // promises "the previous key no longer works". Clearing is_active (not deleting) keeps the
    // record, matching revoke and the rest of the fleet; a failure at either step rolls the whole
    // transaction back, so the customer keeps the old key.
    sqlx::query("UPDATE personal_api_keys SET is_active = false WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(tenant_id)
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?;

    let minted = mint_with_role(&mut tx, tenant_id, user_id, &name, &role).await?;
    tx.commit().await.map_err(AppError::Database)?;

    Ok(Json(json!({
        "id": minted.id.to_string(),
        "rotated_from": id.to_string(),
        "name": minted.name,
        "role": role,
        "key": minted.key,
        "prefix": minted.prefix,
        "note": "The previous key no longer works. This one can be viewed again in Settings after re-entering your password."
    })))
}

#[derive(Deserialize)]
pub struct RevealRequest {
    /// The caller's CURRENT password. A stolen session token must not be enough to read a live
    /// credential — the same rule `profile::change_password` already applies.
    pub password: String,
}

/// POST /api/personal-api-keys/:id/reveal — show the full key again (repeatable).
///
/// David, 2026-11-11: the key must stay retrievable. The plaintext lives in `key_sealed` and is
/// decrypted HERE and nowhere else; authentication still hashes the presented key and compares
/// `key_hash`, so this column can never become an auth shortcut.
///
/// Every reveal is written to `personal_api_key_reveals` (who, when, which prefix, from where) and
/// to the app log — never the secret itself.
pub async fn reveal_key(
    State(s): State<AppState>,
    Extension(c): Extension<Claims>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(req): Json<RevealRequest>,
) -> ApiResult<impl IntoResponse> {
    require_key_admin(&c)?;
    let tenant_id = Uuid::parse_str(&c.aid).map_err(|_| AppError::Unauthorized)?;
    let user_id = Uuid::parse_str(&c.sub).ok();

    let row = sqlx::query(
        "SELECT key_prefix, key_sealed, api_role FROM personal_api_keys
         WHERE id = $1 AND tenant_id = $2 AND is_active = true",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&s.db)
    .await
    .map_err(AppError::Database)?
    .ok_or_else(|| AppError::NotFound("API key not found".into()))?;

    let prefix: String = row.get("key_prefix");
    let sealed: Option<String> = row.get("key_sealed");
    let role: String = row.get("api_role");

    // ── re-authenticate ────────────────────────────────────────────────────────────────────────
    let uid = user_id.ok_or(AppError::Unauthorized)?;
    let stored: Option<String> =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1 AND is_active = true")
            .bind(uid)
            .fetch_optional(&s.db)
            .await
            .map_err(AppError::Database)?;
    let stored = stored.ok_or(AppError::Unauthorized)?;
    if !crate::auth::handlers::verify_password(&req.password, &stored)? {
        return Err(AppError::UnauthorizedReason("Password is incorrect".into()));
    }

    // ── open the sealed copy ──────────────────────────────────────────────────────────────────
    let sealed = sealed.ok_or_else(|| {
        AppError::Conflict(
            "This key was created before keys were stored recoverably, so it cannot be displayed. \
             Regenerate it to get a key you can view and copy."
                .to_string(),
        )
    })?;
    let full_key = crate::secret_box::open(tenant_id, &sealed);
    if full_key.is_empty() {
        // The envelope exists but the server secret in the environment cannot open it. That is an
        // operator problem, not the customer's — never a silent empty string.
        tracing::error!(
            key = %id,
            tenant = %tenant_id,
            "personal API key reveal: sealed copy could not be opened (server secret changed?)"
        );
        return Err(AppError::Internal(
            "This key could not be opened by the server's encryption key. Regenerate it.".into(),
        ));
    }

    // ── audit: the reveal is a security event ────────────────────────────────────────────────
    let ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .or_else(|| {
            headers
                .get("x-real-ip")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.to_string())
        });
    let ua = headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string());
    if let Err(e) = sqlx::query(
        "INSERT INTO personal_api_key_reveals (id, key_id, tenant_id, user_id, key_prefix, ip_address, user_agent)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(Uuid::new_v4())
    .bind(id)
    .bind(tenant_id)
    .bind(user_id)
    .bind(&prefix)
    .bind(&ip)
    .bind(&ua)
    .execute(&s.db)
    .await
    {
        // Never fail the reveal on an audit-write hiccup; make it loud in the log instead.
        tracing::error!(error = %e, key = %id, "personal API key reveal: audit row could not be written");
    }
    tracing::info!(
        key = %id,
        key_prefix = %prefix,
        tenant = %tenant_id,
        user = ?user_id,
        ip = ?ip,
        "personal API key REVEALED"
    );

    Ok(Json(json!({
        "id": id.to_string(),
        "prefix": prefix,
        "role": role,
        "key": full_key
    })))
}

/// One-time backfill: every workspace that exists today gets a key it can view.
///
/// Gated on the `fleet_data_backfills` marker so it runs ONCE. Without that gate a customer who
/// deliberately revoked every key would get a new one minted behind their back on the next boot.
///
/// Rows minted before `key_sealed` existed keep working for authentication but their plaintext is
/// gone for good; they are counted here for hand-off and the UI offers Regenerate instead of View.
pub async fn backfill_missing_keys(db: &sqlx::PgPool) -> Result<(usize, i64), AppError> {
    const MARKER: &str = "personal_api_keys_auto_v1";

    let done: Option<String> =
        sqlx::query_scalar("SELECT name FROM fleet_data_backfills WHERE name = $1")
            .bind(MARKER)
            .fetch_optional(db)
            .await
            .map_err(AppError::Database)?;
    if done.is_some() {
        return Ok((0, 0));
    }

    let mut tx = db.begin().await.map_err(AppError::Database)?;
    // Serialise across instances: two workers booting at once must not mint two keys each.
    sqlx::query("SELECT pg_advisory_xact_lock(7263114001)")
        .execute(&mut *tx)
        .await
        .map_err(AppError::Database)?;

    let tenants: Vec<(Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT t.id,
                (SELECT u.id FROM users u WHERE u.tenant_id = t.id ORDER BY u.created_at LIMIT 1)
         FROM tenants t
         WHERE NOT EXISTS (
            SELECT 1 FROM personal_api_keys k WHERE k.tenant_id = t.id AND k.is_active = true
         )
         ORDER BY t.created_at",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(AppError::Database)?;

    let mut minted = 0usize;
    for (tenant_id, user_id) in tenants {
        mint_on(&mut tx, tenant_id, user_id, "auto-provisioned").await?;
        minted += 1;
    }

    let unrecoverable: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM personal_api_keys WHERE is_active = true AND key_sealed IS NULL",
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(AppError::Database)?;

    sqlx::query(
        "INSERT INTO fleet_data_backfills (name, detail) VALUES ($1, $2)
         ON CONFLICT (name) DO NOTHING",
    )
    .bind(MARKER)
    .bind(format!(
        "minted {minted} key(s) for workspaces with none; {unrecoverable} pre-existing key(s) have no \
         sealed copy (plaintext unrecoverable — regenerate to hand off)"
    ))
    .execute(&mut *tx)
    .await
    .map_err(AppError::Database)?;

    tx.commit().await.map_err(AppError::Database)?;
    Ok((minted, unrecoverable))
}

pub fn router(state: AppState) -> axum::Router<AppState> {
    use axum::routing::{delete, get, post};
    axum::Router::new()
        .route("/", get(list_keys).post(create_key))
        .route("/:id", delete(revoke_key))
        .route("/:id/rotate", post(rotate_key))
        .route("/:id/reveal", post(reveal_key))
        // Plan gating — the admin controls this module per plan
        // (the module & feature registry is the source of truth for the admin UI — see src/module_registry).
        .layer(axum::middleware::from_fn_with_state(
            crate::body_deadline::BodyReadDeadline::from_secs(state.config.body_read_deadline_secs),
            crate::body_deadline::body_read_deadline_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            crate::features::FeatureGate::new(state.db.clone(), "api_access", "API access"),
            crate::features::gate_mw,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::middleware::auth_middleware,
        ))
}
