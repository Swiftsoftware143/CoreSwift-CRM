use axum::{
    extract::{Path, State},
    Extension, Json,
};
use serde_json::json;
use uuid::Uuid;

use super::feature_gate;
use super::models::*;

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

pub async fn add_domain(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<AddDomainRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    feature_gate::check_domain_limit(&state.db, account_id).await?;

    let label = req.label.clone().unwrap_or_else(|| req.domain.clone());

    match req.provider_type.as_str() {
        "smtp" => add_smtp_domain(&state, account_id, &req, &label).await,
        _ => add_mailgun_domain(&state, account_id, &req, &label).await,
    }
}

pub async fn list_domains(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let domains = sqlx::query_as::<_, PrivateEmailDomain>(
        "SELECT * FROM private_email_domains WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(account_id)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::Database)?;

    // t_72f0bc83: the domain list is where a customer answers "is this configured?", so it has to
    // answer with the SAME credential question the send path asks — not with row presence. Before
    // this, a domain whose stored key nobody can open rendered exactly like a healthy one and the
    // only symptom was a failed send ("Stored API key for domain … cannot be read").
    let (keys, provider_keys) = load_credential_maps(&state.db, account_id).await?;

    let out: Vec<serde_json::Value> = domains
        .iter()
        .map(|d| with_credential_status(account_id, d, &keys, &provider_keys))
        .collect();

    Ok(Json(serde_json::json!(out)))
}

/// The two credential stores `send_handler::load_provider_config` resolves a domain's key from, read
/// once per request: `private_email_api_keys` (mailgun) and `provider_api_keys` (everything else).
type CredentialMaps = (
    std::collections::HashMap<Uuid, String>,
    std::collections::HashMap<Uuid, String>,
);

async fn load_credential_maps(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
) -> Result<CredentialMaps, AppError> {
    let keys = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id, api_key_encrypted FROM private_email_api_keys WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_all(db)
    .await
    .map_err(AppError::Database)?
    .into_iter()
    .collect();
    let provider_keys = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id, COALESCE(access_key_encrypted, '') FROM provider_api_keys WHERE tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_all(db)
    .await
    .map_err(AppError::Database)?
    .into_iter()
    .collect();
    Ok((keys, provider_keys))
}

/// Which of `usable | unreadable | empty | missing` this domain's outbound credential is — decided
/// by OPENING the stored value, mirroring `load_provider_config` field for field: `api_key_id` wins
/// when set (mailgun → `private_email_api_keys`, other providers → `provider_api_keys`), otherwise
/// the domain carries its own `smtp_password_encrypted` (smtp) or `mailgun_api_key`.
fn domain_credential_status(
    tenant_id: Uuid,
    d: &PrivateEmailDomain,
    keys: &std::collections::HashMap<Uuid, String>,
    provider_keys: &std::collections::HashMap<Uuid, String>,
) -> &'static str {
    let stored: Option<&String> = match d.api_key_id {
        Some(kid) if d.provider_type == "mailgun" => keys.get(&kid),
        Some(kid) => provider_keys.get(&kid),
        None if d.provider_type == "smtp" => d.smtp_password_encrypted.as_ref(),
        None => {
            if d.mailgun_api_key.trim().is_empty() {
                None
            } else {
                Some(&d.mailgun_api_key)
            }
        }
    };
    match stored {
        None => "missing",
        Some(s) => super::api_keys_handler::stored_key_status(tenant_id, s),
    }
}

/// The domain row as the client sees it, plus the credential verdict (never the credential itself:
/// `PrivateEmailDomain`'s credential columns are `#[serde(skip_serializing)]`).
fn with_credential_status(
    tenant_id: Uuid,
    d: &PrivateEmailDomain,
    keys: &std::collections::HashMap<Uuid, String>,
    provider_keys: &std::collections::HashMap<Uuid, String>,
) -> serde_json::Value {
    let status = domain_credential_status(tenant_id, d, keys, provider_keys);
    let mut v = serde_json::to_value(d).unwrap_or_else(|_| json!({}));
    if let Some(obj) = v.as_object_mut() {
        obj.insert("credential_status".into(), json!(status));
        if status == "unreadable" {
            obj.insert(
                "credential_hint".into(),
                json!(super::api_keys_handler::REENTER_HINT),
            );
        }
    }
    v
}

pub async fn delete_domain(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(domain_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let result = sqlx::query("DELETE FROM private_email_domains WHERE id = $1 AND tenant_id = $2")
        .bind(domain_id)
        .bind(account_id)
        .execute(&state.db)
        .await
        .map_err(AppError::Database)?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Domain not found".into()));
    }

    Ok(Json(json!({"deleted": true})))
}

pub async fn update_domain(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(domain_id): Path<Uuid>,
    Json(req): Json<UpdateDomainRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    if let Some(catch_all) = req.catch_all_enabled {
        if catch_all {
            let allowed = feature_gate::can_enable_catch_all(&state.db, account_id).await?;
            if !allowed {
                return Err(AppError::BadRequest(
                    "Catch-all requires Pro plan or higher".into(),
                ));
            }
        }
    }

    // Re-entering a credential THIS deployment cannot open (t_72f0bc83). Sealed on write through the
    // app-wide envelope, so the row comes back `usable` and the send path stops failing closed. An
    // empty string means "leave the stored value alone", the convention every other credential write
    // in this app follows. Without this route a domain whose key stopped opening was unfixable: the
    // only way to re-add a domain is to delete it, which cascades its mailboxes away.
    let sealed_mailgun = match req.mailgun_api_key.as_deref() {
        Some(k) if !k.trim().is_empty() => Some(crate::secret_box::seal(account_id, k)?),
        _ => None,
    };
    let sealed_smtp = match req.smtp_password.as_deref() {
        Some(k) if !k.trim().is_empty() => Some(crate::secret_box::seal(account_id, k)?),
        _ => None,
    };

    // Re-entering a working key also RE-VERIFIES the domain: Mailgun is asked again, so a row added
    // while its DNS was still propagating does not stay unverified for ever — and therefore does
    // not stay permanently off the delivery path (t_d9d6120a). `None` means "not determined" and the
    // stored value is kept (COALESCE below): a failed lookup must never downgrade a working domain.
    let reverified: Option<bool> = match req.mailgun_api_key.as_deref() {
        Some(k) if !k.trim().is_empty() => {
            let meta: Option<(String, String)> = sqlx::query_as(
                "SELECT domain, mailgun_region FROM private_email_domains WHERE id = $1 AND tenant_id = $2",
            )
            .bind(domain_id)
            .bind(account_id)
            .fetch_optional(&state.db)
            .await
            .map_err(AppError::Database)?;
            match meta {
                Some((domain, region)) => mailgun_domain_state(k, &domain, &region)
                    .await
                    .map(|s| s.eq_ignore_ascii_case("active")),
                None => None,
            }
        }
        _ => None,
    };

    // A saved key can only be bound if it belongs to this tenant — otherwise a domain could be
    // pointed at another tenant's ciphertext, which by construction never opens for this one.
    if let Some(kid) = req.api_key_id {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM private_email_api_keys WHERE id = $1 AND tenant_id = $2)",
        )
        .bind(kid)
        .bind(account_id)
        .fetch_one(&state.db)
        .await
        .map_err(AppError::Database)?;
        if !exists {
            return Err(AppError::NotFound("Saved API key not found".into()));
        }
    }

    let row = sqlx::query_as::<_, PrivateEmailDomain>(
        r#"
        UPDATE private_email_domains
        SET catch_all_enabled = COALESCE($3, catch_all_enabled),
            mailgun_api_key = COALESCE($4, mailgun_api_key),
            smtp_password_encrypted = COALESCE($5, smtp_password_encrypted),
            api_key_id = COALESCE($6, api_key_id),
            verified = COALESCE($7, verified),
            updated_at = NOW()
        WHERE id = $1 AND tenant_id = $2
        RETURNING *
        "#,
    )
    .bind(domain_id)
    .bind(account_id)
    .bind(req.catch_all_enabled)
    .bind(sealed_mailgun)
    .bind(sealed_smtp)
    .bind(req.api_key_id)
    .bind(reverified)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    match row {
        Some(domain) => {
            let (keys, provider_keys) = load_credential_maps(&state.db, account_id).await?;
            Ok(Json(with_credential_status(
                account_id,
                &domain,
                &keys,
                &provider_keys,
            )))
        }
        None => Err(AppError::NotFound("Domain not found".into())),
    }
}

/// Which credentials may answer "what does the provider think of this domain?" — the row's own
/// `mailgun_api_key` first, because that is the one the DELIVERY gate reads
/// (`communications::providers::private_mailgun_byok` selects `mailgun_api_key` and requires it
/// non-empty), then the saved key `api_key_id` points at, because that is the one the tab's
/// credential cell reports. Each is opened with `secret_box::open` (the app-wide reader, which
/// never fails); an empty result means this deployment cannot read it and the caller is told so.
async fn recheck_credentials(
    db: &sqlx::PgPool,
    tenant_id: Uuid,
    own_key: &str,
    api_key_id: Option<Uuid>,
) -> Result<Vec<String>, AppError> {
    let mut candidates: Vec<String> = Vec::new();
    let own = crate::secret_box::open(tenant_id, own_key);
    if !own.trim().is_empty() {
        candidates.push(own);
    }
    if let Some(kid) = api_key_id {
        let stored: Option<String> = sqlx::query_scalar(
            "SELECT api_key_encrypted FROM private_email_api_keys WHERE id = $1 AND tenant_id = $2",
        )
        .bind(kid)
        .bind(tenant_id)
        .fetch_optional(db)
        .await
        .map_err(AppError::Database)?;
        if let Some(sealed) = stored {
            let opened = crate::secret_box::open(tenant_id, &sealed);
            if !opened.trim().is_empty() && !candidates.iter().any(|c| c == &opened) {
                candidates.push(opened);
            }
        }
    }
    Ok(candidates)
}

/// `POST /api/private-email/domains/:id/verify` — ask the provider again, now.
///
/// The hole this closes (t_f02ade57, residual from t_d9d6120a): `verified` is what lets this
/// workspace's own domain carry its mail, and it was only ever derived from Mailgun's answer at
/// ADD time or when a credential was RE-ENTERED. A tenant who added `mail.theirdomain.com` while
/// Mailgun still reported it `unverified` (DNS/DKIM propagating) stayed `verified = false` for ever
/// once Mailgun flipped the domain to `active` — their mail kept leaving on the platform transport
/// while the product told them to add a domain they had already added, and the only way out was to
/// re-type an API key that was never wrong. This is the explicit re-check: no credential re-entry,
/// no extra call on the delivery path.
///
/// Direction, deliberately: PROMOTE ONLY. Mailgun's `active` is the only state that sets
/// `verified = true`, and a re-check never clears the column. A re-check is a repair action the
/// customer presses, so it must not be able to take a working sending identity away — DNS state is
/// polled from a third party over the network, and one ambiguous read must not knock mail off a
/// domain that is carrying it (the same reason `update_domain` COALESCEs `verified`). The provider's
/// verdict is always returned as `mailgun_state`, so a surface can show what Mailgun really says.
/// Demotion still happens on the credential re-entry path, where the human is already asserting the
/// configuration is wrong.
pub async fn verify_domain(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(domain_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let row: Option<(String, String, String, String, Option<Uuid>, bool)> = sqlx::query_as(
        r#"SELECT domain, mailgun_region, provider_type, mailgun_api_key, api_key_id, verified
             FROM private_email_domains
            WHERE id = $1 AND tenant_id = $2"#,
    )
    .bind(domain_id)
    .bind(account_id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    let (domain, region, provider_type, own_key, api_key_id, verified_before) =
        row.ok_or_else(|| AppError::NotFound("Domain not found".into()))?;

    // An SMTP domain has no provider-side state to read: its credential is a mailbox, and a
    // "re-check" there would be a lie dressed as a repair.
    if provider_type != "mailgun" {
        return Err(AppError::BadRequest(
            "A re-check only applies to a Mailgun domain — this domain sends over SMTP, so there is \
             no provider state to re-read."
                .into(),
        ));
    }

    let candidates = recheck_credentials(&state.db, account_id, &own_key, api_key_id).await?;
    if candidates.is_empty() {
        return Err(AppError::Validation(
            "This domain's stored Mailgun API key cannot be read — re-enter it above, then \
             re-check."
                .into(),
        ));
    }

    // `None` from every candidate means Mailgun did not answer for this domain with this
    // credential at all (wrong region, revoked key, or the domain is not on that account) — an
    // undetermined state, which is NOT the same as "unverified" and must leave the row alone.
    let mut mailgun_state: Option<String> = None;
    for key in &candidates {
        if let Some(s) = mailgun_domain_state(key, &domain, &region).await {
            mailgun_state = Some(s);
            break;
        }
    }
    let mailgun_state = match mailgun_state {
        Some(s) => s,
        None => {
            return Err(AppError::BadRequest(format!(
                "Mailgun did not answer for {} with the stored credential (wrong region, revoked \
                 key, or the domain is not on that account). Nothing was changed — this domain is \
                 still marked {}.",
                domain, verified_before
            )))
        }
    };

    let now_active = mailgun_state.eq_ignore_ascii_case("active");
    let verified_after = verified_before || now_active;
    let changed = verified_after != verified_before;

    // One statement, so two concurrent re-checks cannot interleave a read and a write: `updated_at`
    // only moves when the verdict actually changed.
    let row = sqlx::query_as::<_, PrivateEmailDomain>(
        r#"
        UPDATE private_email_domains
        SET verified = $3,
            updated_at = CASE WHEN $4 THEN NOW() ELSE updated_at END
        WHERE id = $1 AND tenant_id = $2
        RETURNING *
        "#,
    )
    .bind(domain_id)
    .bind(account_id)
    .bind(verified_after)
    .bind(changed)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?
    .ok_or_else(|| AppError::NotFound("Domain not found".into()))?;

    if changed {
        tracing::info!(
            tenant = %account_id,
            domain = %domain,
            mailgun_state = %mailgun_state,
            "Mailgun domain state re-checked — this domain now carries the workspace's mail"
        );
    } else {
        tracing::info!(
            tenant = %account_id,
            domain = %domain,
            mailgun_state = %mailgun_state,
            verified = verified_after,
            "Mailgun domain state re-checked — no change"
        );
    }

    let (keys, provider_keys) = load_credential_maps(&state.db, account_id).await?;
    let mut body = with_credential_status(account_id, &row, &keys, &provider_keys);
    if let Some(obj) = body.as_object_mut() {
        obj.insert("mailgun_state".into(), json!(mailgun_state));
        obj.insert("verified_before".into(), json!(verified_before));
        obj.insert("changed".into(), json!(changed));
    }
    Ok(Json(body))
}

/// Mailgun's own verdict on a domain: `Some(state)` when this key can read the domain on the
/// account, `None` when it cannot (wrong key, wrong region, or the domain is not on the account).
///
/// `state` is the provider's word, not ours: `active` means Mailgun's own DNS/ownership checks
/// passed, which is exactly what `private_email_domains.verified` is supposed to mean. Reading it
/// here is what makes that column reachable — before t_d9d6120a nothing in the app ever wrote
/// `verified = true`, so the delivery path's Private Email layer could never fire for anyone.
async fn mailgun_domain_state(api_key: &str, domain: &str, region: &str) -> Option<String> {
    let base_url = if region == "eu" {
        "https://api.eu.mailgun.net"
    } else {
        "https://api.mailgun.net"
    };

    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{}/v3/domains/{}", base_url, domain))
        .basic_auth("api", Some(api_key))
        .send()
        .await
        .inspect_err(
            |e| tracing::warn!(error = %e, domain = %domain, "Mailgun domain lookup failed"),
        )
        .ok()?;
    if !resp.status().is_success() {
        tracing::warn!(
            status = resp.status().as_u16(),
            domain = %domain,
            "Mailgun does not serve this domain for this key"
        );
        return None;
    }
    resp.json::<serde_json::Value>()
        .await
        .inspect_err(|e| tracing::warn!(error = %e, domain = %domain, "Mailgun domain payload did not parse"))
        .ok()?
        .get("domain")
        .and_then(|d| d.get("state"))
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
}

async fn add_mailgun_domain(
    state: &AppState,
    account_id: Uuid,
    req: &AddDomainRequest,
    label: &str,
) -> ApiResult<Json<serde_json::Value>> {
    let (encrypted_key, api_key_id): (String, Option<Uuid>) = if let Some(kid) = req.api_key_id {
        let row = sqlx::query_as::<_, (String,)>(
            "SELECT api_key_encrypted FROM private_email_api_keys WHERE id = $1 AND tenant_id = $2",
        )
        .bind(kid)
        .bind(account_id)
        .fetch_optional(&state.db)
        .await
        .map_err(AppError::Database)?
        .ok_or_else(|| AppError::NotFound("Saved API key not found".into()))?;
        (row.0, Some(kid))
    } else if let Some(ref raw_key) = req.mailgun_api_key {
        // Sealed with the app-wide envelope (`enc:v1:`), not the bare AES-GCM body this path used
        // to write: one format for every credential column (t_45772522).
        let encrypted = crate::secret_box::seal(account_id, raw_key)?;
        let kid = sqlx::query_as::<_, (Uuid,)>(
            r#"
            INSERT INTO private_email_api_keys (tenant_id, label, provider, api_key_encrypted)
            VALUES ($1, $2, 'mailgun', $3)
            ON CONFLICT DO NOTHING
            RETURNING id
            "#,
        )
        .bind(account_id)
        .bind(label)
        .bind(&encrypted)
        .fetch_optional(&state.db)
        .await
        .map_err(AppError::Database)?;
        (encrypted, kid.map(|(id,)| id))
    } else {
        return Err(AppError::BadRequest(
            "Either mailgun_api_key or api_key_id is required".into(),
        ));
    };

    // `secret_box::open` is the app-wide reader: it understands an `enc:v1:` row, the older
    // prefixless ciphertext and a legacy plaintext row, and it never fails. An empty result
    // therefore means THIS DEPLOYMENT cannot read the stored credential — a configuration problem
    // the caller can act on, reported instead of a bare 500 (t_45772522).
    let raw_key = crate::secret_box::open(account_id, &encrypted_key);
    if raw_key.trim().is_empty() {
        return Err(AppError::Validation(
            "This domain's stored Mailgun API key cannot be read — re-add the domain with a valid \
             key."
                .into(),
        ));
    }

    // NOT named `state`: that would shadow the `&AppState` parameter this function needs below.
    let domain_state = match mailgun_domain_state(&raw_key, &req.domain, &req.mailgun_region).await
    {
        Some(state) => state,
        None => {
            return Err(AppError::BadRequest(
                "Invalid Mailgun API key or domain not configured in Mailgun".into(),
            ))
        }
    };
    // `verified` is recorded from Mailgun's answer, not assumed: only `active` means its DNS and
    // ownership checks passed, and only a verified domain may carry this workspace's mail
    // (t_d9d6120a). A domain that is still `unverified` on the account is added exactly as before,
    // it simply does not take over delivery yet.
    let verified = domain_state.eq_ignore_ascii_case("active");

    let row = sqlx::query_as::<_, PrivateEmailDomain>(
        r#"
        INSERT INTO private_email_domains (tenant_id, domain, mailgun_api_key, mailgun_region, label, api_key_id, provider_type, verified)
        VALUES ($1, $2, $3, $4, $5, $6, 'mailgun', $7)
        RETURNING *
        "#,
    )
    .bind(account_id)
    .bind(&req.domain)
    .bind(&encrypted_key)
    .bind(&req.mailgun_region)
    .bind(label)
    .bind(api_key_id)
    .bind(verified)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(serde_json::to_value(&row).unwrap()))
}

async fn add_smtp_domain(
    state: &AppState,
    account_id: Uuid,
    req: &AddDomainRequest,
    label: &str,
) -> ApiResult<Json<serde_json::Value>> {
    let smtp_host = req
        .smtp_host
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("smtp_host is required for SMTP provider".into()))?;
    let smtp_username = req.smtp_username.as_deref().ok_or_else(|| {
        AppError::BadRequest("smtp_username is required for SMTP provider".into())
    })?;
    let smtp_password = req.smtp_password.as_deref().ok_or_else(|| {
        AppError::BadRequest("smtp_password is required for SMTP provider".into())
    })?;

    let encrypted_smtp_password = crate::secret_box::seal(account_id, smtp_password)?;

    // Store an EMPTY mailgun_api_key for backward compat (column is NOT NULL). This used to be
    // `encrypt_api_key(account_id, "")`, i.e. a 28-byte ciphertext of nothing, which made "no key
    // configured" indistinguishable from "a key is configured" for every reader (t_45772522).
    let empty_key = String::new();

    let row = sqlx::query_as::<_, PrivateEmailDomain>(
        r#"
        INSERT INTO private_email_domains (
            tenant_id, domain, mailgun_api_key, mailgun_region, label,
            provider_type, smtp_host, smtp_port, smtp_username, smtp_password_encrypted, smtp_tls,
            inbound_mode
        )
        VALUES ($1, $2, $3, 'us', $4, 'smtp', $5, $6, $7, $8, $9, 'none')
        RETURNING *
        "#,
    )
    .bind(account_id)
    .bind(&req.domain)
    .bind(&empty_key)
    .bind(label)
    .bind(smtp_host)
    .bind(req.smtp_port.unwrap_or(587))
    .bind(smtp_username)
    .bind(&encrypted_smtp_password)
    .bind(req.smtp_tls)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::Database)?;

    Ok(Json(serde_json::to_value(&row).unwrap()))
}
