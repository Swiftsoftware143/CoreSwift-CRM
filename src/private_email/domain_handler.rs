use axum::{
    extract::{Path, State},
    Extension, Json,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::feature_gate;
use super::models::*;

/// The mail client's own host. A customer's BRANDED address (`inbox.<their-domain>`) is a CNAME to
/// this name, Proxied at THEIR Cloudflare, so their own certificate carries the padlock. Nothing
/// here renders an IP address, and the mail itself never passes through this host — only the
/// screens do (the records that carry mail are the customer's own Mailgun ones).
pub const MAIL_CLIENT_HOST: &str = "inbox.coreswiftcrm.com";

/// Whether `inbox.<domain>` really opens our mail client right now.
///
/// Three answers, because "not working" is not one state to a customer:
///   * `Active`    — the name answers and serves our client: the branded front door works;
///   * `Waiting`   — the name does not resolve yet: the CNAME is not added (or DNS has not spread);
///   * `Attention` — the name resolves but serves something else (wrong target, or their Cloudflare
///     is refusing the connection, which is what SSL/TLS mode "Full (strict)" does to an origin
///     certificate that does not carry their hostname).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Branded {
    Active,
    Waiting,
    Attention,
}

impl Branded {
    pub fn as_str(self) -> &'static str {
        match self {
            Branded::Active => "active",
            Branded::Waiting => "waiting",
            Branded::Attention => "attention",
        }
    }
}

/// One probe per domain per minute: the setup page and the inbox tab both ask, and Mailgun's own
/// record read is already a network round trip — this must not add a second one per render.
fn branded_cache() -> &'static Mutex<HashMap<String, (Instant, Branded)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, Branded)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A short-TTL cached verdict on `inbox.<domain>`.
pub async fn branded_state(domain: &str) -> Branded {
    let key = domain.trim().to_ascii_lowercase();
    if let Ok(cache) = branded_cache().lock() {
        if let Some((at, state)) = cache.get(&key) {
            if at.elapsed() < Duration::from_secs(60) {
                return *state;
            }
        }
    }
    let state = probe_branded(&key).await;
    if let Ok(mut cache) = branded_cache().lock() {
        cache.insert(key, (Instant::now(), state));
    }
    state
}

async fn probe_branded(domain: &str) -> Branded {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .redirect(reqwest::redirect::Policy::limited(3))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Branded::Waiting,
    };
    let url = format!("https://inbox.{}/", domain);
    match client
        .get(&url)
        .header("User-Agent", "CoreSwift-setup-check")
        .send()
        .await
    {
        Ok(resp) => {
            let code = resp.status().as_u16();
            // 525/526 are Cloudflare's "cannot reach the origin over TLS" pair: the record is
            // there and their edge is answering, so this is a setting to fix, not a record to add.
            if code == 525 || code == 526 {
                return Branded::Attention;
            }
            if !resp.status().is_success() {
                return Branded::Attention;
            }
            match resp.text().await {
                // The mail client under the branded name — the whole point of the record.
                Ok(body) if body.contains("CoreSwift Mail") => Branded::Active,
                _ => Branded::Attention,
            }
        }
        // No answer at all: the name does not resolve, so the record is not added yet.
        Err(_) => Branded::Waiting,
    }
}

/// The provider's LIVE verdict on a stored domain, for the create-mailbox gate.
///
/// `private_email_domains.verified` is only what the last check saw; a customer who finished their
/// DNS after adding the domain would be refused with "not receiving mail yet" while Mailgun already
/// says `active`. The gate asks the provider before refusing — and only then. `None` means the
/// provider could not be read at all, which is not a verdict and never becomes one.
pub async fn domain_is_active(
    pool: &sqlx::PgPool,
    tenant_id: Uuid,
    domain: &PrivateEmailDomain,
) -> Option<bool> {
    // SendGrid publishes no API that answers "is this domain pointed at me", so the honest
    // measurement is the same DNS read the setup page and the re-check make: the domain's MX must
    // name mx.sendgrid.net. Returning the STORED flag here made this door read the value it was
    // supposed to refresh (kanban t_54b5d21b): a customer whose MX had just gone live was refused
    // with "not receiving mail yet" until they pressed Check again, while the records endpoint —
    // measuring the same DNS — had already told them everything checked out.
    if domain.inbound_provider == "sendgrid" {
        return sendgrid_mx_ready(&domain.domain).await;
    }
    if domain.inbound_provider != "mailgun" {
        // A sending-only domain has no receiver at all, so there is nothing to measure and the
        // stored flag is returned as its own answer rather than invented here.
        return Some(domain.verified);
    }
    let candidates =
        recheck_credentials(pool, tenant_id, &domain.mailgun_api_key, domain.api_key_id)
            .await
            .ok()?;
    for key in &candidates {
        if let Some(state) = mailgun_domain_state(key, &domain.domain, &domain.mailgun_region).await
        {
            return Some(state.eq_ignore_ascii_case("active"));
        }
    }
    None
}

use crate::auth::models::Claims;
use crate::errors::{ApiResult, AppError};
use crate::AppState;

/// The TWO independent choices a domain makes (kanban t_04db9228): how its mail LEAVES, and how it
/// ARRIVES. They used to be one field, which meant a customer who wanted to keep their own sender
/// was told to move their inbound mail — and vice versa.
///
/// `sending` maps to `provider_type`; `receiving` maps to `inbound_provider`. A combination such as
/// sending through Amazon SES (over SMTP) while receiving through Mailgun is supported on purpose:
/// it is the combination that removes the "move everything to Mailgun" objection.
struct DomainChoices {
    sending: String,
    receiving: String,
}

impl DomainChoices {
    fn resolve(req: &AddDomainRequest) -> ApiResult<Self> {
        // `sending_provider` is the new name; `provider_type` is the field every existing caller
        // (and the older panel) posts, so it is the fallback rather than a second meaning.
        let sending = req
            .sending_provider
            .as_deref()
            .unwrap_or(&req.provider_type)
            .trim()
            .to_ascii_lowercase();
        if !matches!(sending.as_str(), "mailgun" | "smtp") {
            return Err(AppError::BadRequest(format!(
                "'{}' is not a way to send mail here. Choose Mailgun, or SMTP — that covers \
                 Amazon SES, SendGrid, Postmark, Google Workspace and any other mail service.",
                sending
            )));
        }

        // Absent means "the same as before today": a Mailgun domain receives through Mailgun, an
        // SMTP one has no receiver. Nothing that already exists changes behaviour.
        let receiving = match req
            .receiving_provider
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
        {
            Some(r) => r.to_ascii_lowercase(),
            None if sending == "smtp" => "none".to_string(),
            None => "mailgun".to_string(),
        };
        if !matches!(receiving.as_str(), "mailgun" | "sendgrid" | "none") {
            return Err(AppError::BadRequest(format!(
                "'{}' is not a way to receive mail here. Mail can arrive through Mailgun or \
                 SendGrid, and 'none' means this domain only sends.",
                receiving
            )));
        }

        Ok(Self { sending, receiving })
    }

    fn inbound_mode(&self) -> &'static str {
        if self.receiving == "none" {
            "none"
        } else {
            "webhook"
        }
    }
}

/// A fresh 32-hex credential for the SendGrid Inbound Parse URL. Unguessable rather than random for
/// its own sake: SendGrid signs nothing, so this string in the URL IS the authentication.
fn new_inbound_token() -> String {
    Uuid::new_v4().simple().to_string()
}

pub async fn add_domain(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<AddDomainRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    crate::auth::middleware::require_account_admin_for(
        &claims,
        "add, verify or remove a sending domain",
    )?;
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    feature_gate::check_domain_limit(&state.db, account_id).await?;

    let label = req.label.clone().unwrap_or_else(|| req.domain.clone());
    let choices = DomainChoices::resolve(&req)?;

    // A DOMAIN ARMED TO RECEIVE MUST CARRY THE CREDENTIAL THAT AUTHENTICATES A DELIVERY (2026-11-27).
    // `receiving_provider` DEFAULTS to Mailgun for a Mailgun-sending domain, and the Mailgun door
    // treated the signing key as optional — so a register call that omitted it stored a row with
    // `inbound_mode = 'webhook'` and `inbound_provider = 'mailgun'` and no key at all. The receiver
    // fails closed, so every delivery to that domain was refused with "no signing key is configured",
    // and the workspace believed it had an inbox that silently dropped mail. Measured live that
    // morning: seven domains registered through the account-key door were ALL in exactly that state,
    // and one delivery had already been refused. The SMTP door already refused this combination; the
    // rule lives here now so both doors answer one sentence. A domain that only SENDS says so with
    // `receiving_provider = "none"` (and can arm receiving later, with its key, from the domain row).
    let has_webhook_key = req
        .webhook_signing_key
        .as_deref()
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false);
    if choices.receiving == "mailgun" && !has_webhook_key {
        return Err(AppError::BadRequest(
            "Receiving through Mailgun needs its webhook signing key (Mailgun → Settings → API \
             keys → HTTP webhook signing key). Without it every delivery is refused, so this \
             domain is not stored until it is supplied. Choose \"nowhere\" for a domain that only \
             sends, and add receiving later with its key."
                .into(),
        ));
    }

    match choices.sending.as_str() {
        "smtp" => add_smtp_domain(&state, account_id, &req, &label, &choices).await,
        _ => add_mailgun_domain(&state, account_id, &req, &label, &choices).await,
    }
}

/// MAIL READINESS — "can this workspace actually send and receive, and what is missing if not".
///
/// WHY THIS EXISTS (David, 2026-11-20): the three mail identities were real but invisible, so a new
/// workspace reached a feature that needs mail, the feature quietly did nothing useful, and the honest
/// conclusion was "the system doesn't work". A gate needs a single truthful answer to ask, so this
/// answers per lane: what it is, whether it is ready, and — when it is not — the exact reason in the
/// words a non-technical owner can act on.
///
/// The lane rules are the app's own, not a second opinion:
///   * ordinary (system) mail leaves from the OLDEST verified Mailgun domain;
///   * automation leaves from `mg.<that domain>` and cannot exist without it;
///   * inbound needs a verified domain AND a readable signing key, because the receiver fails closed.
///
/// `can_block` is a property of the LANE, not a measurement: system mail still goes out on the shared
/// platform identity, so it only warns; automation and inbound have no fallback at all, so a missing
/// piece stops them. `blocked_now` is the measurement — the lanes that are NOT ready at this instant —
/// and only `blocked_now` may be read as an active fault. Before this, the lane field was called
/// `blocking` and was a constant, so a reader saw `true` beside `any_blocking: false` and had no way
/// to tell a capability from a fault.
///
/// `identity_source` names the credential that actually carries a lane, so a green lane cannot hide a
/// missing credential: `own_domain` (the account's own verified sending domain), `own_key` (the
/// domain's own 32-character webhook signing key), `platform` (the shared platform credential) or
/// `none`. Inbound carried by `platform` is genuinely receiving, but not with the customer's own key,
/// so it says `platform` and its `reason`/`action` name the key that is still unset instead of
/// reporting "Nothing to do.".
pub async fn readiness(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    #[derive(sqlx::FromRow)]
    struct Row {
        domain: String,
        verified: bool,
        provider_type: Option<String>,
        has_key: bool,
    }

    let rows: Vec<Row> = sqlx::query_as(
        // Oldest first: the server's own rule for "which domain is the account's main sender"
        // (providers::account_main_domain) is ORDER BY created_at ASC, and the answer must match it.
        "SELECT domain, COALESCE(verified, false) AS verified, provider_type, \
                (webhook_signing_key_encrypted IS NOT NULL \
                 AND length(webhook_signing_key_encrypted) > 0) AS has_key \
           FROM private_email_domains WHERE tenant_id = $1 ORDER BY created_at ASC",
    )
    .bind(account_id)
    .fetch_all(&state.db)
    .await
    .map_err(AppError::Database)?;

    let is_mailgun = |r: &Row| r.provider_type.as_deref().unwrap_or("mailgun") == "mailgun";
    let main = rows
        .iter()
        .find(|r| r.verified && is_mailgun(r))
        .map(|r| r.domain.clone());
    // Inbound needs a verified Mailgun domain AND a credential for the receiver: either the domain's
    // own signing key, or the platform webhook token the forward URL can carry (the provider's key is
    // account-wide and dashboard-only, so a workspace that never pasted it can still receive).
    let platform_token = crate::communications::platform_mail_config::stored(&state.db)
        .await
        .and_then(|row| crate::communications::platform_mail_config::field(&row, "webhook_token"))
        .map(|t| t.trim().len() >= 24)
        .unwrap_or(false);
    // A KEY THAT CANNOT WORK IS NOT READY (2026-11-20). Presence is not the question: a 50-character
    // SENDING API key stored in this field looked healthy everywhere and refused every delivery, so this
    // gate would have said "ready" about a door that never opens. The stored value is opened and its shape
    // checked exactly as the save path checks it (32 hexadecimal characters).
    let mut keyed_and_usable = false;
    let mut stored_wrong_shape: Vec<String> = Vec::new();
    for r in rows
        .iter()
        .filter(|r| r.verified && is_mailgun(r) && r.has_key)
    {
        let sealed: Option<String> = sqlx::query_scalar(
            "SELECT webhook_signing_key_encrypted FROM private_email_domains \
              WHERE tenant_id = $1 AND domain = $2",
        )
        .bind(account_id)
        .bind(&r.domain)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
        match sealed {
            Some(sealed) => {
                let opened = crate::secret_box::open(account_id, &sealed);
                let k = opened.trim();
                if k.len() == 32 && k.chars().all(|c| c.is_ascii_hexdigit()) {
                    keyed_and_usable = true;
                } else {
                    stored_wrong_shape.push(r.domain.clone());
                }
            }
            None => stored_wrong_shape.push(r.domain.clone()),
        }
    }
    let awaiting_key: Vec<String> = rows
        .iter()
        .filter(|r| r.verified && is_mailgun(r) && !r.has_key)
        .map(|r| r.domain.clone())
        .collect();

    let automation_domain = main.as_ref().map(|d| format!("mg.{d}"));
    // ONE decision, from the pure helper below and proved by its unit tests: each lane's `ready`, the
    // credential that carries inbound, and the lanes blocked right now cannot drift apart.
    let state = lane_state(main.is_some(), keyed_and_usable, platform_token);
    let system_ready = state.system_ready;
    let automation_ready = state.automation_ready;
    let inbound_ready = state.inbound_ready;
    let inbound_identity_source = state.inbound_identity_source;
    let blocked_now = state.blocked_now;

    let setup_hint =
        "Open Email Domains, add your domain, add the records it shows, press Re-check, \
                      then Set key with your provider's HTTP webhook signing key.";

    // The inbox lane's words. Two ready-adjacent states are honest: NOT ready (nothing can
    // authenticate a delivery) and ready-on-the-platform-credential (deliveries authenticate, but not
    // with this workspace's own key). The second used to render as reason:null + "Nothing to do.",
    // which told the operator the lane was finished while its own key was still unset.
    let (inbox_reason, inbox_action): (serde_json::Value, String) = if !inbound_ready {
        let reason = if !stored_wrong_shape.is_empty() {
            format!(
                "Receiving is refused for {}: the stored signing key is not the 32-character webhook key, so no delivery can be authenticated. Remove the key (receiving then works on the platform credential) or paste the correct one.",
                stored_wrong_shape.join(", ")
            )
        } else if awaiting_key.is_empty() {
            "No verified domain yet, so nothing can arrive.".to_string()
        } else {
            format!(
                "Mail sent to {} is refused on arrival: the inbound signing key is not set, and the receiver fails closed rather than accept a delivery it cannot authenticate.",
                awaiting_key.join(", ")
            )
        };
        (json!(reason), setup_hint.to_string())
    } else if inbound_identity_source.starts_with("platform") {
        let named = if awaiting_key.is_empty() {
            main.clone().unwrap_or_default()
        } else {
            awaiting_key.join(", ")
        };
        let reason = if !stored_wrong_shape.is_empty() {
            format!(
                "Receiving works on the platform credential: the signing key stored for {} is not the 32-character webhook key, so it cannot authenticate a delivery and the platform token carries this lane instead.",
                stored_wrong_shape.join(", ")
            )
        } else {
            format!(
                "Receiving works on the platform credential, not on this workspace's own key: no signing key of its own is set for {named}, so deliveries are authenticated by the platform webhook token."
            )
        };
        let action = if !stored_wrong_shape.is_empty() {
            "Paste the correct 32-character webhook signing key for that domain (or remove the unusable one), so receiving stops depending on the platform credential.".to_string()
        } else {
            format!(
                "Optional: press Set key on that domain and paste its own webhook signing key, so receiving stops depending on the platform credential."
            )
        };
        (json!(reason), action)
    } else {
        (serde_json::Value::Null, "Nothing to do.".to_string())
    };

    let lanes = json!([
        {
            "lane": "system",
            "title": "Ordinary mail (receipts, invitations, password resets, replies)",
            "ready": system_ready,
            "can_block": false,
            "identity_source": if system_ready { "own_domain" } else { "platform" },
            "identity": main.as_ref().map(|d| format!("noreply@{d}")),
            "reason": if system_ready { serde_json::Value::Null } else { json!(
                "No verified domain yet, so these messages leave on the shared platform identity — your customer sees our name, not yours, and your reputation is not what is being built."
            ) },
            "action": if system_ready { "Nothing to do." } else { "Add and verify your own domain to send receipts under your own name." },
        },
        {
            "lane": "automation",
            "title": "Automation mail (rules, sequences, campaigns)",
            "ready": automation_ready,
            "can_block": true,
            "identity_source": if automation_ready { "own_domain" } else { "none" },
            "identity": automation_domain,
            "reason": if automation_ready { serde_json::Value::Null } else { json!(
                "An automation has no sending identity without a verified domain: there is no base domain to build the automation subdomain from, so every automation email is refused before it leaves."
            ) },
            "action": if automation_ready { "Nothing to do." } else { setup_hint },
        },
        {
            "lane": "inbox",
            "title": "Inbound mail (the shared inbox, replying as support@)",
            "ready": inbound_ready,
            "can_block": true,
            "identity_source": inbound_identity_source,
            "identity": main.as_ref().map(|d| format!("anything@{d}")),
            "reason": inbox_reason,
            "action": inbox_action,
            "domains_awaiting_key": awaiting_key,
        },
    ]);

    Ok(Json(json!({
        "lanes": lanes,
        "main_domain": main,
        "automation_domain": automation_domain,
        "blocked_now": blocked_now,
        "any_blocking": !blocked_now.is_empty(),
        "domain_count": rows.len(),
    })))
}

/// The mail-lane decision in one place, pure so it can be tested.
///
/// `ready` and `blocked_now` MUST come from the same booleans — the defect this replaces was a static
/// `blocking` constant rendered beside a computed `any_blocking`, so a capability read as an active
/// fault. `identity_source` names the credential that actually carries inbound: `own_key` (the domain's
/// own 32-character signing key), `platform` (the shared platform webhook token standing in for a key
/// that was never set) or `none`. Automation's readiness is the same question as system's: it is built
/// from the account's own verified domain, so no separate domain can make it ready.
struct LaneState {
    system_ready: bool,
    automation_ready: bool,
    inbound_ready: bool,
    inbound_identity_source: &'static str,
    blocked_now: Vec<&'static str>,
}

fn lane_state(main_present: bool, keyed_and_usable: bool, platform_token: bool) -> LaneState {
    // RED, NOT GREEN, ON THE PLATFORM FALLBACK (CEO ruling, 2026-11-27). Readiness used to be
    // `keyed_and_usable || (platform_token && main_present)`, so a domain awaiting its OWN signing key
    // reported ready:true with identity_source "platform" — a green lane whose mail authenticates with
    // somebody else's credential. For govcon correspondence that is a compliance failure, not a warning.
    // A lane is ready only when a key of its own can authenticate the delivery; the platform fallback
    // is still reported, but as the reason it is NOT ready.
    let inbound_ready = keyed_and_usable;
    let inbound_identity_source = if keyed_and_usable {
        "own_key"
    } else if platform_token && main_present {
        // Kept so the UI can say what WOULD carry mail - but it no longer makes the lane ready.
        "platform_fallback_not_ready"
    } else {
        "none"
    };
    let mut blocked_now: Vec<&'static str> = Vec::new();
    if !main_present {
        blocked_now.push("system");
    }
    if !main_present {
        blocked_now.push("automation");
    }
    if !inbound_ready {
        blocked_now.push("inbox");
    }
    LaneState {
        system_ready: main_present,
        automation_ready: main_present,
        inbound_ready,
        inbound_identity_source,
        blocked_now,
    }
}

#[cfg(test)]
mod readiness_lane_tests {
    use super::lane_state;

    #[test]
    fn a_workspace_with_no_domain_blocks_and_names_every_lane() {
        let s = lane_state(false, false, false);
        assert!(!s.system_ready && !s.automation_ready && !s.inbound_ready);
        assert_eq!(s.inbound_identity_source, "none");
        assert_eq!(s.blocked_now, vec!["system", "automation", "inbox"]);
    }

    #[test]
    fn a_domain_without_its_own_key_is_RED_and_names_the_platform_fallback() {
        // The previous name of this test was `..._is_ready_and_names_the_platform`, and it asserted the
        // exact behaviour the CEO measured as a false green: a domain awaiting its own signing key read
        // ready:true. Readiness is now RED, the fallback is named as the reason, and the lane lands in
        // blocked_now so any_blocking agrees with it.
        let s = lane_state(true, false, true);
        assert!(!s.inbound_ready);
        assert_eq!(s.inbound_identity_source, "platform_fallback_not_ready");
        assert!(s.blocked_now.contains(&"inbox"));
    }

    #[test]
    fn a_domain_with_its_own_key_is_carried_by_that_key() {
        let s = lane_state(true, true, false);
        assert!(s.inbound_ready);
        assert_eq!(s.inbound_identity_source, "own_key");
        assert!(s.blocked_now.is_empty());
    }

    #[test]
    fn a_platform_token_cannot_carry_a_lane_without_a_domain() {
        let s = lane_state(false, false, true);
        assert!(!s.inbound_ready);
        assert_eq!(s.inbound_identity_source, "none");
        assert!(s.blocked_now.contains(&"inbox"));
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

    // Does this workspace's CURRENT plan sell catch-all? Read once for the whole list, by the same
    // helper the inbound router and the switch's own gate use (kanban t_6b54961b), so a domain that
    // still carries the stored preference on a downgraded plan is reported as off-plan instead of
    // reading "on" while every unmatched delivery is refused.
    let plan_sells_catch_all = feature_gate::catch_all_granted(&state.db, account_id).await?;

    // The upgrade door is a catalogue read, so it is fetched ONCE for the list — and only when some
    // row actually carries a switch the plan is refusing (kanban t_f2234ffa).
    let catch_all_upgrade = if !plan_sells_catch_all && domains.iter().any(|d| d.catch_all_enabled)
    {
        Some(feature_gate::catch_all_upgrade_clause(&state.db).await)
    } else {
        None
    };

    let out: Vec<serde_json::Value> = domains
        .iter()
        .map(|d| {
            with_credential_status(
                account_id,
                d,
                &keys,
                &provider_keys,
                plan_sells_catch_all,
                catch_all_upgrade.as_deref(),
            )
        })
        .collect();

    // Is each domain's BRANDED front door (`inbox.<domain>`) really serving the mail client? The
    // inbox tab opens its LOG IN button on that name, so the tab has to know whether it works now or
    // whether the customer should be sent to the shared host while the CNAME spreads. Cached for a
    // minute per domain, so a list render cannot turn into one network round trip per row.
    let mut out = out;
    for (i, d) in domains.iter().enumerate() {
        let state = branded_state(&d.domain).await;
        if let Some(obj) = out[i].as_object_mut() {
            obj.insert("branded".into(), json!(state.as_str()));
            obj.insert(
                "branded_url".into(),
                json!(format!("https://inbox.{}", d.domain)),
            );
        }
    }

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
/// `PrivateEmailDomain`'s credential columns are `#[serde(skip_serializing)]`), plus the inbound
/// webhook key's verdict (kanban t_fbd607a7).
fn with_credential_status(
    tenant_id: Uuid,
    d: &PrivateEmailDomain,
    keys: &std::collections::HashMap<Uuid, String>,
    provider_keys: &std::collections::HashMap<Uuid, String>,
    plan_sells_catch_all: bool,
    catch_all_upgrade: Option<&str>,
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
        // The inbound webhook signing key, reported the way every other credential in this app is:
        // by OPENING the stored value, then answering presence + LENGTH. Never the value, never the
        // ciphertext. A domain with no key is not "broken" — it is a sending-only domain — but its
        // inbound mail IS refused (t_7f134e77), so the panel has to be able to say which state it is
        // in. `unreadable` is the one that needs an action: the stored key cannot be opened by this
        // deployment, so no delivery will ever validate until it is re-entered.
        let (wh_status, wh_len) = webhook_key_status(tenant_id, d);
        obj.insert("webhook_signing_key_status".into(), json!(wh_status));
        obj.insert("webhook_signing_key_length".into(), json!(wh_len));

        // Honesty about a paid feature the current plan no longer sells (kanban t_6b54961b). The
        // stored column can read `true` while the inbound router refuses every unmatched address,
        // so a panel driven by the column alone would claim catch-all is on for a workspace that is
        // not entitled to it. The stored preference stays in `catch_all_enabled` (that is what the
        // switch remembers and what an upgrade restores); the EFFECTIVE answer and a sentence sit
        // beside it, decided by the very same rule the router applies.
        let stored = d.catch_all_enabled;
        let off_plan = stored && !plan_sells_catch_all;
        obj.insert(
            "catch_all_effective".into(),
            json!(super::feature_gate::catch_all_effective(
                stored,
                plan_sells_catch_all
            )),
        );
        obj.insert("catch_all_off_plan".into(), json!(off_plan));
        if off_plan {
            // The tier named here is READ from the plan catalogue by the caller
            // (`feature_gate::catch_all_upgrade_clause`, kanban t_f2234ffa) — the same sentence the
            // switch's own 400 uses. A hardcoded name is what made this hint and the refusal drift
            // from `plans.features`; there is one clause now, so they cannot.
            obj.insert(
                "catch_all_hint".into(),
                json!(format!(
                    "Catch-all is off: your current plan does not include it, so mail to an \
                     address that does not exist on this domain is refused. Upgrade to {} to \
                     restore it — this switch is remembered.",
                    catch_all_upgrade.unwrap_or("a higher plan")
                )),
            );
        }
    }
    v
}

/// The response shape every domain surface returns, with the catch-all door built from the plan
/// catalogue — but ONLY for a row whose stored switch is actually being refused. That is one extra
/// query on the downgraded case and none on the ordinary one (kanban t_f2234ffa).
async fn domain_json(
    state: &AppState,
    tenant_id: Uuid,
    d: &PrivateEmailDomain,
    keys: &HashMap<Uuid, String>,
    provider_keys: &HashMap<Uuid, String>,
    plan_sells_catch_all: bool,
) -> serde_json::Value {
    let clause = if d.catch_all_enabled && !plan_sells_catch_all {
        Some(feature_gate::catch_all_upgrade_clause(&state.db).await)
    } else {
        None
    };
    with_credential_status(
        tenant_id,
        d,
        keys,
        provider_keys,
        plan_sells_catch_all,
        clause.as_deref(),
    )
}

/// `usable | unreadable | missing` + the length of the DECRYPTED signing key (0 when there is none).
/// `missing` is the "no key configured" state (NULL or empty), which the receiver treats as "refuse
/// every delivery"; `unreadable` means a key IS stored but no key on this box can open it.
fn webhook_key_status(tenant_id: Uuid, d: &PrivateEmailDomain) -> (&'static str, usize) {
    let stored = d.webhook_signing_key_encrypted.as_deref().unwrap_or("");
    if stored.trim().is_empty() {
        return ("missing", 0);
    }
    let opened = crate::secret_box::open(tenant_id, stored);
    if opened.trim().is_empty() {
        ("unreadable", 0)
    } else {
        ("usable", opened.chars().count())
    }
}

pub async fn delete_domain(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(domain_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    crate::auth::middleware::require_account_admin_for(
        &claims,
        "add, verify or remove a sending domain",
    )?;
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
    crate::auth::middleware::require_account_admin_for(
        &claims,
        "add, verify or remove a sending domain",
    )?;
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    if let Some(catch_all) = req.catch_all_enabled {
        if catch_all {
            let allowed = feature_gate::can_enable_catch_all(&state.db, account_id).await?;
            if !allowed {
                // The plan named here is READ from the catalogue (kanban t_f2234ffa), never typed.
                // Two hardcoded versions of this sentence were both wrong on this install: "Pro plan
                // or higher" named `pro` after it was deactivated (it is `is_active = false`), and
                // "Professional plan or higher" is right only while Professional happens to be the
                // cheapest active tier that sells catch-all. `catch_all_refusal` asks
                // `plans`/`plan_modules` — active, priced cheapest-first, feature sold — so the
                // sentence cannot drift from what a customer can actually buy, and it carries the
                // price so the answer names the door instead of sending them to hunt for it.
                return Err(AppError::BadRequest(
                    feature_gate::catch_all_refusal(&state.db).await,
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

    // The inbound webhook signing key (kanban t_fbd607a7). `Some(None)` is an explicit JSON null —
    // the ONLY way to clear the column, because this request's other fields all use "empty/omitted
    // means leave the stored value alone" and the panel posts every field on save. `Some(Some(v))`
    // with a non-empty v rotates the key; an empty string is the same "leave as is" signal the
    // sibling credential fields use, so submitting the panel's blank input cannot disarm the
    // receiver by accident. Restated in the request-struct doc comment on
    // `UpdateDomainRequest::webhook_signing_key`.
    let (webhook_touch, sealed_webhook): (bool, Option<String>) = match req
        .webhook_signing_key
        .as_ref()
    {
        None => (false, None),
        Some(None) => (true, None),
        Some(Some(k)) if k.trim().is_empty() => (false, None),
        // TRIM BEFORE SEALING (measured 2026-11-20): a key pasted from a dashboard very often carries a
        // trailing newline or a leading space, and HMAC validation then fails with the useless sentence
        // "the configured signing key did not validate" while the stored value LOOKS right. The value is
        // a token, never padded, so trimming it is safe and removes a whole class of silent mismatch.
        Some(Some(k)) => {
            let trimmed = k.trim();
            // MEASURED, not guessed (2026-11-20): the provider's HTTP webhook signing key is exactly 32
            // hexadecimal characters. Its SENDING API key is 50 characters shaped `32hex-8hex-8hex`, and
            // pasting THAT here was accepted, shown as "usable", and then refused every delivery on
            // arrival. A length test alone does not catch it, so the SHAPE is enforced: 32 hex or nothing.
            let is_signing_key =
                trimmed.len() == 32 && trimmed.chars().all(|c| c.is_ascii_hexdigit());
            if !is_signing_key {
                return Err(AppError::Validation(
                    "That is not the HTTP webhook signing key. It is 32 hexadecimal characters — in your \
                     provider: Settings, then API Security, then HTTP webhook signing key. The sending API \
                     key (50 characters, with dashes) will not authenticate deliveries. Leave this blank \
                     to keep receiving on the platform credential."
                        .into(),
                ));
            }
            (true, Some(crate::secret_box::seal(account_id, trimmed)?))
        }
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

    // ── Switching HOW this domain RECEIVES (kanban t_04db9228) ────────────────────────────────
    // Read the two receiving columns, decide the new three values in Rust, then write them: an SQL
    // CASE that has to tell "absent" from "set to the same value" is the kind of expression that
    // looks right and quietly clears a column. Sending is untouched by any of this.
    let receiving = match req
        .receiving_provider
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        Some(r) => {
            let r = r.to_ascii_lowercase();
            if !matches!(r.as_str(), "mailgun" | "sendgrid" | "none") {
                return Err(AppError::BadRequest(format!(
                    "'{}' is not a way to receive mail here. Mail can arrive through Mailgun or \
                     SendGrid, and 'none' means this domain only sends.",
                    r
                )));
            }
            Some(r)
        }
        None => None,
    };
    let current: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT inbound_provider, inbound_token, webhook_signing_key_encrypted \
           FROM private_email_domains WHERE id = $1 AND tenant_id = $2",
    )
    .bind(domain_id)
    .bind(account_id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;
    let (current_provider, current_token, current_hook_key) =
        current.ok_or_else(|| AppError::NotFound("Domain not found".into()))?;
    let next_provider = receiving.clone().unwrap_or(current_provider);

    // HOW IT SENDS (kanban B208). Three real choices, each with code behind it: Mailgun's API,
    // SMTP.com's HTTP API, and `smtp_relay` — the customer's own mail server. Anything else is
    // refused here rather than stored, because a provider token with no arm behind it would leave
    // every send failing with "unknown email provider" while the panel claimed the domain was set.
    if let Some(p) = req.sending_provider.as_deref().map(str::trim) {
        if !matches!(p, "mailgun" | "smtp" | "smtp_relay") {
            return Err(AppError::Validation(
                "sending_provider must be one of: mailgun, smtp, smtp_relay.".to_string(),
            ));
        }
        if p == "smtp_relay" {
            let host_now = req
                .smtp_host
                .as_deref()
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .map(str::to_string);
            if host_now.is_none() {
                // Refused, not saved-and-unusable: a relay with no host makes every automation send
                // fail, and the panel would look configured.
                // `smtp_host` is NULL on a Mailgun domain, so the column decodes as an Option — asking for a
                // plain String answered 500 "unexpected null" (measured live 2026-11-21).
                let stored: Option<String> = sqlx::query_scalar::<_, Option<String>>(
                    "SELECT smtp_host FROM private_email_domains WHERE id = $1 AND tenant_id = $2",
                )
                .bind(domain_id)
                .bind(account_id)
                .fetch_optional(&state.db)
                .await
                .map_err(AppError::Database)?
                .flatten();
                if stored.unwrap_or_default().trim().is_empty() {
                    return Err(AppError::Validation(
                        "Sending through your own mail server needs its host — for example \
                         smtp.postmarkapp.com. Nothing was saved."
                            .to_string(),
                    ));
                }
            }
        }
    }

    // THE SECOND DOOR (kanban t_04db9228): adding a Mailgun-receiving domain without its signing key
    // is refused on the create path, so switching an existing domain TO Mailgun without one must be
    // refused the same way — otherwise the switch succeeds, the panel says "receiving: Mailgun", and
    // every delivery is then refused by the receiver with a message about a key nobody was asked for.
    // ONLY WHEN RECEIVING IS BEING SWITCHED (measured live 2026-11-21): this guard runs on
    // `next_provider`, which equals the CURRENT value when the request says nothing about receiving.
    // So changing only the SENDING transport was refused for want of an inbound webhook key, on a
    // domain that already receives fine on the platform token. A choice nobody made must not require
    // a credential.
    if receiving.is_some() && next_provider == "mailgun" {
        let stored_key_works = current_hook_key
            .as_deref()
            .filter(|k| !k.trim().is_empty())
            .map(|k| !crate::secret_box::open(account_id, k).trim().is_empty())
            .unwrap_or(false);
        let supplying_now = webhook_touch && sealed_webhook.is_some();
        if !stored_key_works && !supplying_now {
            return Err(AppError::BadRequest(
                "Receiving through Mailgun needs its webhook signing key (Mailgun → Send → \
                 Domains → your domain → Webhook signing key). Send it in this same save, or the \
                 switch would leave the domain receiving nothing."
                    .into(),
            ));
        }
    }
    let next_token = if next_provider == "sendgrid" && current_token.is_none() {
        Some(new_inbound_token())
    } else {
        current_token
    };
    let next_mode = if next_provider == "none" {
        "none"
    } else {
        "webhook"
    };

    let row = sqlx::query_as::<_, PrivateEmailDomain>(
        r#"
        UPDATE private_email_domains
        SET catch_all_enabled = COALESCE($3, catch_all_enabled),
            mailgun_api_key = COALESCE($4, mailgun_api_key),
            smtp_password_encrypted = COALESCE($5, smtp_password_encrypted),
            api_key_id = COALESCE($6, api_key_id),
            verified = COALESCE($7, verified),
            webhook_signing_key_encrypted = CASE
                WHEN $8 THEN $9::text ELSE webhook_signing_key_encrypted END,
            inbound_provider = $10,
            inbound_mode = $11,
            inbound_token = $12,
            provider_type = COALESCE($13, provider_type),
            smtp_host = COALESCE($14, smtp_host),
            smtp_port = COALESCE($15, smtp_port),
            smtp_username = COALESCE($16, smtp_username),
            smtp_tls = COALESCE($17, smtp_tls),
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
    .bind(webhook_touch)
    .bind(sealed_webhook)
    .bind(&next_provider)
    .bind(next_mode)
    .bind(&next_token)
    // The five sending columns (kanban B208): a trimmed, non-empty value replaces what is stored;
    // empty/omitted keeps it, the same convention every credential write in this app follows.
    .bind(
        req.sending_provider
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty()),
    )
    .bind(
        req.smtp_host
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty()),
    )
    .bind(req.smtp_port)
    .bind(
        req.smtp_username
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty()),
    )
    .bind(req.smtp_tls)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    match row {
        Some(domain) => {
            let (keys, provider_keys) = load_credential_maps(&state.db, account_id).await?;
            // Same plan question the list asks, so the updated row answers with the same honest
            // effective state (kanban t_6b54961b). A PATCH that switches catch-all ON on an
            // unentitled plan was already refused above; this matters for the DOWNGRADED row, whose
            // stored value survives a plan change while the entitlement does not.
            let plan_sells_catch_all =
                feature_gate::catch_all_granted(&state.db, account_id).await?;
            Ok(Json(
                domain_json(
                    &state,
                    account_id,
                    &domain,
                    &keys,
                    &provider_keys,
                    plan_sells_catch_all,
                )
                .await,
            ))
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
    crate::auth::middleware::require_account_admin_for(
        &claims,
        "add, verify or remove a sending domain",
    )?;
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    // Sending is deliberately NOT read here: a re-check answers for the RECEIVER, and asking the
    // sending provider is the confusion this card exists to remove.
    let row: Option<(String, String, String, Option<Uuid>, bool, String)> = sqlx::query_as(
        r#"SELECT domain, mailgun_region, mailgun_api_key, api_key_id, verified,
                  inbound_provider
             FROM private_email_domains
            WHERE id = $1 AND tenant_id = $2"#,
    )
    .bind(domain_id)
    .bind(account_id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    let (domain, region, own_key, api_key_id, verified_before, inbound_provider) =
        row.ok_or_else(|| AppError::NotFound("Domain not found".into()))?;

    // ── The RECEIVING provider is what a re-check re-reads (kanban t_04db9228) ────────────────
    // SendGrid has no API that answers "is this domain pointed at me", so the measurement is DNS:
    // the domain's MX must name mx.sendgrid.net. Promotion only, exactly like the Mailgun arm — a
    // re-check must never be able to take a working receiving identity away on one ambiguous read.
    if inbound_provider == "sendgrid" {
        let mx_ready = sendgrid_mx_ready(&domain).await;
        let mx_ready = match mx_ready {
            Some(v) => v,
            None => {
                return Err(AppError::BadRequest(format!(
                    "Could not read the DNS for {} right now, so nothing was changed — this domain \
                     is still marked {}.",
                    domain, verified_before
                )))
            }
        };
        let verified_after = verified_before || mx_ready;
        let changed = verified_after != verified_before;
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

        tracing::info!(
            tenant = %account_id,
            domain = %domain,
            mx_points_at_sendgrid = mx_ready,
            verified = verified_after,
            "SendGrid receiving re-checked through DNS"
        );

        let (keys, provider_keys) = load_credential_maps(&state.db, account_id).await?;
        let plan_sells_catch_all = feature_gate::catch_all_granted(&state.db, account_id).await?;
        let mut body = domain_json(
            &state,
            account_id,
            &row,
            &keys,
            &provider_keys,
            plan_sells_catch_all,
        )
        .await;
        if let Some(obj) = body.as_object_mut() {
            obj.insert("receiving_provider".into(), json!("sendgrid"));
            obj.insert("mx_points_at_sendgrid".into(), json!(mx_ready));
            obj.insert("verified_before".into(), json!(verified_before));
            obj.insert("changed".into(), json!(changed));
            obj.insert(
                "mailgun_state".into(),
                json!(if mx_ready { "active" } else { "waiting" }),
            );
        }
        return Ok(Json(body));
    }

    // A sending-only domain has no receiver, so a "re-check" there would be a lie dressed as a
    // repair. (An SMTP-SENDING domain whose receiver IS Mailgun is re-checked above like any other
    // Mailgun receiver — sending and receiving are separate choices.)
    if inbound_provider != "mailgun" {
        return Err(AppError::BadRequest(
            "This domain only sends mail — no mail arrives through us on it, so there is no \
             receiving state to re-read. Choose Mailgun or SendGrid for receiving first."
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
    let plan_sells_catch_all = feature_gate::catch_all_granted(&state.db, account_id).await?;
    let mut body = domain_json(
        &state,
        account_id,
        &row,
        &keys,
        &provider_keys,
        plan_sells_catch_all,
    )
    .await;
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

/// Is SendGrid really the destination for this domain's mail right now?
///
/// SendGrid publishes no API that answers this for a domain the customer configured in THEIR
/// account, so the honest measurement is DNS: the domain's MX must name `mx.sendgrid.net`. Asked
/// over DNS-over-HTTPS because `dig` is not installed on this box, and an empty `dig` would read as
/// "no records" when it is really "command not found".
///
/// `None` means DNS could not be read at all — an undetermined answer, which must never be reported
/// as "not set up" (that sends a customer chasing a record that is already live).
async fn sendgrid_mx_ready(domain: &str) -> Option<bool> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
        .ok()?;
    for url in [
        format!("https://dns.google/resolve?name={}&type=MX", domain),
        format!(
            "https://cloudflare-dns.com/dns-query?name={}&type=MX",
            domain
        ),
    ] {
        let resp = match client
            .get(&url)
            .header("Accept", "application/dns-json")
            .header("User-Agent", "CoreSwift-setup-check")
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => r,
            _ => continue,
        };
        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let answers = match body.get("Answer").and_then(|a| a.as_array()) {
            Some(a) => a,
            // A NODATA answer (the name exists, no MX) is a real verdict: nothing points at SendGrid.
            None if body.get("Status").and_then(|s| s.as_i64()) == Some(0) => return Some(false),
            None => continue,
        };
        let ready = answers.iter().any(|a| {
            a.get("data")
                .and_then(|d| d.as_str())
                .map(|d| d.to_ascii_lowercase().contains("sendgrid.net"))
                .unwrap_or(false)
        });
        return Some(ready);
    }
    None
}

/// The setup checklist for a domain that RECEIVES through SendGrid (kanban t_04db9228).
///
/// Two rows are ours to give: the MX record that points the domain at SendGrid, and the exact URL
/// SendGrid must post to (which carries this domain's own token — the credential). DKIM/SPF are
/// NOT ours in this shape: they belong to whichever service SENDS the mail, so a customer sending
/// through Amazon SES must not be handed a SendGrid SPF include they do not need. When the same
/// domain also sends through Mailgun, Mailgun's sending records ARE shown, because they are the
/// customer's real ones and we can read them.
async fn sendgrid_records(
    state: &AppState,
    account_id: Uuid,
    domain_id: Uuid,
    domain: &str,
    verified: bool,
    token: Option<&str>,
    sending_provider: &str,
    mailgun_region: &str,
    own_key: &str,
    api_key_id: Option<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let mx_ready = sendgrid_mx_ready(domain).await;

    let mut receiving = vec![json!({
        "section": "receiving",
        "type": "MX",
        "name": domain,
        "value": "mx.sendgrid.net",
        "priority": 10,
        "valid": mx_ready,
        "purpose": "Tells the internet to deliver your mail to SendGrid, which hands each message straight to this inbox.",
        "note": "DNS only (grey cloud) if your domain is on Cloudflare — the orange proxy cannot carry mail.",
    })];

    // The destination URL is a CREDENTIAL, so it is only shown to the account that owns it, and only
    // here. Without it the customer cannot arm receiving at all: SendGrid Inbound Parse is configured
    // by pasting one URL, and nothing else identifies this inbox to SendGrid.
    let parse_url = token.map(|t| {
        format!(
            "{}/api/v1/webhooks/sendgrid/inbound/{}",
            crate::email::APP_URL,
            t
        )
    });
    if let Some(url) = parse_url.as_ref() {
        receiving.push(json!({
            "section": "receiving",
            "type": "URL",
            "name": format!("SendGrid Inbound Parse for {}", domain),
            "value": url,
            "priority": serde_json::Value::Null,
            "valid": serde_json::Value::Null,
            "purpose": "Paste this as the destination in SendGrid → Settings → Inbound Parse, tick \"POST the raw, full MIME message\" OFF, and point it at this domain. It is this inbox's own address — anyone holding it can post mail to you, so treat it like a password.",
            "note": "This URL is the only credential SendGrid can carry: their Inbound Parse signs nothing. Do not share it.",
        }));
    }

    // Sending side. Mailgun is readable from the account; anything else (SES, SendGrid, Postmark,
    // Google) is a service we do not hold the credential for, and inventing its DKIM values would be
    // worse than saying where they come from.
    let mut sending: Vec<serde_json::Value> = Vec::new();
    let mut sending_unknown_by_us = false;
    if sending_provider == "mailgun" {
        if let Ok(candidates) =
            recheck_credentials(&state.db, account_id, own_key, api_key_id).await
        {
            let mut detail: Option<serde_json::Value> = None;
            for key in &candidates {
                if let Some(d) = mailgun_domain_detail(key, domain, mailgun_region).await {
                    detail = Some(d);
                    break;
                }
            }
            if let Some(detail) = detail {
                let mut rows = Vec::new();
                flatten_records(
                    "sending",
                    detail.get("sending_dns_records").and_then(|v| v.as_array()),
                    "Proves your mail really comes from you, so it is not marked as spam.",
                    &mut rows,
                );
                sending = rows;
            } else {
                sending_unknown_by_us = true;
            }
        } else {
            sending_unknown_by_us = true;
        }
    } else if sending_provider == "smtp" {
        sending_unknown_by_us = true;
    }
    if sending_unknown_by_us {
        // NO invented records: one honest row that says where the real ones live.
        sending.push(json!({
            "section": "sending",
            "type": "INFO",
            "name": "Your sending records",
            "value": "Provided by the service that sends your mail (Amazon SES, SendGrid, Postmark, Google Workspace)",
            "priority": serde_json::Value::Null,
            "valid": serde_json::Value::Null,
            "checked_by_provider": false,
            "purpose": "Nothing to add here: those SPF and DKIM records are minted by your sending service and must stay exactly as it gave them. This inbox only receives.",
        }));
    }
    sending.push(json!({
        "section": "sending",
        "type": "TXT",
        "name": format!("_dmarc.{}", domain),
        "value": format!("v=DMARC1; p=none; rua=mailto:dmarc@{}", domain),
        "priority": serde_json::Value::Null,
        "valid": serde_json::Value::Null,
        "checked_by_provider": false,
        "purpose": "Tells receiving servers what to do with mail that fails your sending proof. Gmail and Yahoo expect this on a new domain.",
    }));

    let checked_sending: Vec<&serde_json::Value> = sending
        .iter()
        .filter(|r| r["checked_by_provider"] != json!(false) && r["valid"] != json!(null))
        .collect();
    let sending_ok = checked_sending
        .iter()
        .all(|r| r["valid"].as_bool() == Some(true));
    // `mx_checked: null` means DNS could not be read at all — an undetermined answer, not a
    // verdict. It must not unlock the create control either: the write door refuses it too.
    let receiving_ok = mx_ready == Some(true);

    let branded = branded_state(domain).await;
    let branded_row = json!({
        "section": "branded",
        "type": "CNAME",
        "name": format!("inbox.{}", domain),
        "value": MAIL_CLIENT_HOST,
        "priority": serde_json::Value::Null,
        "valid": branded == Branded::Active,
        "attention": branded == Branded::Attention,
        "checked_by_provider": false,
        "measured": true,
        "proxied_required": true,
        "purpose": "Your branded front door: this one makes inbox.<your domain> open your mail, with the padlock on the address.",
        "note": "Set this record to Proxied (orange cloud). Cloudflare then serves the padlock for your address. Your mail records above stay grey (DNS only) — proxying those breaks mail.",
    });

    // Promote only, exactly like the Mailgun arm: the stored column is what the last check saw,
    // and one unreadable DNS answer must never take a working receiving identity away.
    let verified_live = verified || mx_ready == Some(true);
    // The flat list the older panel renders, and the three groups the setup page renders, from the
    // same rows — one source, so the two surfaces cannot disagree.
    let all_records: Vec<serde_json::Value> = receiving
        .iter()
        .chain(sending.iter())
        .chain(std::iter::once(&branded_row))
        .cloned()
        .collect();
    Ok(Json(json!({
        "domain": domain,
        "domain_id": domain_id,
        "receiving_provider": "sendgrid",
        "sending_provider": sending_provider,
        "verified": verified_live,
        "state": if verified_live { "active" } else { "waiting" },
        "records": all_records,
        "groups": [
            { "key": "receiving", "title": "Receiving — so your mail lands in CoreSwift", "records": receiving },
            { "key": "sending", "title": "Sending proof — so your replies are not marked as spam", "records": sending },
            { "key": "branded", "title": "Your branded address — the padlock on your own name", "records": [branded_row] },
        ],
        "receiving_ok": receiving_ok,
        "sending_ok": sending_ok,
        "branded": branded.as_str(),
        "branded_url": format!("https://inbox.{}", domain),
        "mail_client_url": format!("https://{}", MAIL_CLIENT_HOST),
        "inbound_parse_url": parse_url,
        // The same answer `provision_mailbox` computes through `domain_is_active`, so the
        // sentence the page shows and the write it invites cannot disagree (t_54b5d21b).
        "can_create_mailbox": verified_live && sending_ok,
        "mailgun_state": serde_json::Value::Null,
        "mx_checked": mx_ready,
    })))
}

/// GET /api/private-email/domains/:id/records — the records the customer must add, in plain English.
///
/// The setup page teaches a customer how to point their own domain at this service (B205 "customer
/// setup page": CoreSwift does not host their DNS and does not send their mail — each customer keeps
/// their domain where it lives and brings their own Mailgun account). The record VALUES therefore
/// have to be Mailgun's own, read from the account with the stored key, never invented or copied from
/// a doc that may be out of date.
///
/// Each row says, in one sentence, what the record does — MX = where mail is delivered, SPF = proves
/// we may send for you, DKIM = signs your mail — because the page's rule is no jargon and no dead
/// ends.
/// THE ADDRESS A CUSTOMER PASTES INTO THEIR PROVIDER (David, 2026-11-20).
///
/// Every account needs this, not just the one being set up by hand: the receiver is one endpoint for the
/// whole app, and it works out the account from the recipient address — so the SAME URL is correct for
/// every domain and every workspace. It is served from the app rather than hardcoded in a screen so a
/// deployment on another host answers with its own address.
pub fn inbound_webhook_url() -> String {
    let base = std::env::var("APP_PUBLIC_URL")
        .or_else(|_| std::env::var("PUBLIC_APP_URL"))
        .unwrap_or_else(|_| "https://app.coreswiftcrm.com".to_string());
    format!(
        "{}/api/v1/webhooks/mailgun/inbound",
        base.trim_end_matches('/')
    )
}

pub async fn domain_records(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
    Path(domain_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let account_id = Uuid::parse_str(&claims.aid).map_err(|_| AppError::Unauthorized)?;

    let row: Option<(
        String,
        String,
        String,
        Option<Uuid>,
        String,
        bool,
        String,
        Option<String>,
        bool,
    )> = sqlx::query_as(
        r#"SELECT domain, mailgun_region, mailgun_api_key, api_key_id, provider_type, verified,
                      inbound_provider, inbound_token,
                      COALESCE(webhook_signing_key_encrypted, '') <> '' AS has_signing_key
             FROM private_email_domains WHERE id = $1 AND tenant_id = $2"#,
    )
    .bind(domain_id)
    .bind(account_id)
    .fetch_optional(&state.db)
    .await
    .map_err(AppError::Database)?;

    let (
        domain,
        region,
        own_key,
        api_key_id,
        provider_type,
        verified,
        inbound_provider,
        inbound_token,
        signing_key_configured,
    ) = row.ok_or_else(|| AppError::NotFound("Domain not found".into()))?;

    // RECEIVING is what decides which records a customer has to add (kanban t_04db9228). The old
    // check asked the SENDING provider, which is why a domain sending over SMTP was told "nothing
    // here needs adding" even when its mail arrived through us.
    match inbound_provider.as_str() {
        "sendgrid" => {
            return sendgrid_records(
                &state,
                account_id,
                domain_id,
                &domain,
                verified,
                inbound_token.as_deref(),
                &provider_type,
                &region,
                &own_key,
                api_key_id,
            )
            .await
        }
        "mailgun" => {}
        _ => {
            return Err(AppError::BadRequest(
                "This domain only sends mail — no mail arrives through us on it, so there are no \
                 records to add here. Choose a receiving provider (Mailgun or SendGrid) on this \
                 domain if you want an inbox on it."
                    .into(),
            ))
        }
    }

    let candidates = recheck_credentials(&state.db, account_id, &own_key, api_key_id).await?;
    if candidates.is_empty() {
        return Err(AppError::Validation(
            "This domain's stored Mailgun key cannot be read — re-enter it, then check again."
                .into(),
        ));
    }

    let mut detail: Option<serde_json::Value> = None;
    for key in &candidates {
        if let Some(d) = mailgun_domain_detail(key, &domain, &region).await {
            detail = Some(d);
            break;
        }
    }
    let detail = detail.ok_or_else(|| {
        AppError::BadRequest(format!(
            "Mailgun did not answer for {} with the stored credential, so its records cannot be \
             shown. Nothing was changed.",
            domain
        ))
    })?;

    // Mailgun returns two arrays of {record_type, name, value, priority?, valid}. They are
    // flattened into one list with a purpose sentence, because that is what the page renders.
    let mut records = Vec::new();
    flatten_records(
        "receiving",
        detail
            .get("receiving_dns_records")
            .and_then(|v| v.as_array()),
        "Tells the internet where to deliver your mail.",
        &mut records,
    );
    flatten_records(
        "sending",
        detail.get("sending_dns_records").and_then(|v| v.as_array()),
        "Proves your mail really comes from you, so it is not marked as spam.",
        &mut records,
    );

    // ── The two groups the setup page renders (David 2026-11-11, BYOK) ────────────────────────
    // Mailgun returns two arrays; the page shows them as the customer's checklist, grouped by what
    // each group BUYS them, plus the DMARC row Mailgun does not return. EVERY record here belongs to
    // the customer's own provider (their Mailgun, their DNS) — nothing this app renders hands out our
    // server's address or a CNAME to us: their domain never points at our infrastructure for mail,
    // and there is no certificate of ours in the path.
    // A row whose name Mailgun leaves as "@" (the v4 shape for an apex MX) is rendered as the
    // customer's own domain: "@" is DNS shorthand a business owner should not have to decode on an
    // onboarding page, and every other row already names its host in full.
    for r in records.iter_mut() {
        if r["name"].as_str().map(|n| n.trim()) == Some("@") {
            r["name"] = json!(domain);
        }
    }

    let receiving: Vec<serde_json::Value> = records
        .iter()
        .filter(|r| r["section"] == "receiving")
        .cloned()
        .collect();
    let mut sending: Vec<serde_json::Value> = records
        .iter()
        .filter(|r| r["section"] == "sending")
        .cloned()
        .collect();
    // A domain Mailgun returns NO records for cannot be assessed from that list (some domains are
    // verified at the provider without the API echoing their DNS entries). That is not "not ready":
    // the provider's own `active` state decides, and the caller still sees an honest checklist. A
    // domain with records is held to them — one invalid record keeps the create button closed.
    let receiving_ok =
        receiving.is_empty() || receiving.iter().all(|r| r["valid"].as_bool() == Some(true));

    // DMARC is not in Mailgun's DNS answer, and Gmail/Yahoo bulk-sender rules expect it. Without it
    // a brand-new domain's first sends get filtered and the customer blames the inbox. It is
    // reported with `valid: null` because Mailgun does not check it — the page must not claim a
    // verdict it did not measure.
    sending.push(json!({
        "section": "sending",
        "type": "TXT",
        "name": format!("_dmarc.{}", domain),
        "value": format!("v=DMARC1; p=none; rua=mailto:dmarc@{}", domain),
        "priority": serde_json::Value::Null,
        "valid": serde_json::Value::Null,
        "checked_by_provider": false,
        "purpose": "Tells receiving servers what to do with mail that fails your sending proof. Gmail and Yahoo expect this on a new domain.",
    }));
    let checked_sending: Vec<&serde_json::Value> = sending
        .iter()
        .filter(|r| r["checked_by_provider"] != json!(false))
        .collect();
    let sending_ok = checked_sending.is_empty()
        || checked_sending
            .iter()
            .all(|r| r["valid"].as_bool() == Some(true));

    // ── The BRANDED front door: `inbox.<their-domain>` (David 2026-11-11) ────────────────────────
    // Mailgun knows nothing about this one: it is the customer's own mail SCREEN, not a delivery
    // record. It is a CNAME to our mail client, Proxied at their Cloudflare so THEIR certificate
    // carries the padlock — which is why it is the one record on this page that must be orange-cloud,
    // the opposite of every mail record above it. Its status is MEASURED (the app asks the name for
    // the mail client), never assumed: a customer who has not added it must see "Waiting", and one
    // whose Cloudflare refuses the connection must see "Needs attention" instead of a dead link.
    let branded = branded_state(&domain).await;
    let branded_row = json!({
        "section": "branded",
        "type": "CNAME",
        "name": format!("inbox.{}", domain),
        "value": MAIL_CLIENT_HOST,
        "priority": serde_json::Value::Null,
        "valid": branded == Branded::Active,
        "attention": branded == Branded::Attention,
        "checked_by_provider": false,
        "measured": true,
        "proxied_required": true,
        "purpose": "Your branded front door: this one makes inbox.<your domain> open your mail, with the padlock on the address.",
        "note": "Set this record to Proxied (orange cloud). Cloudflare then serves the padlock for your address. Your mail records above stay grey (DNS only) — proxying those breaks mail.",
    });
    records.push(branded_row.clone());

    // Mailgun's own `active` is the live answer; the stored column is only what the last check saw.
    // The create button must not stay shut on a domain Mailgun has since activated (the customer
    // would press Check again and be told everything passes while the button stayed grey).
    let provider_state = detail
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let verified_live = verified || provider_state.eq_ignore_ascii_case("active");

    Ok(Json(json!({
        "domain": domain,
        "domain_id": domain_id,
        "verified": verified_live,
        "state": provider_state,
        "records": records,
        // The setup page's checklist, in the order the customer must do it.
        "groups": [
            { "key": "receiving", "title": "Receiving — so your mail lands in CoreSwift", "records": receiving },
            { "key": "sending", "title": "Sending proof — so your replies are not marked as spam", "records": sending },
            { "key": "branded", "title": "Your branded address — the padlock on your own name", "records": [branded_row] },
        ],
        "receiving_ok": receiving_ok,
        "sending_ok": sending_ok,
        // ── WHERE TO POST THE MAIL (David, 2026-11-20) ────────────────────────────────────────────────
        // The records above make mail ARRIVE at Mailgun; this URL is what turns the arrived mail into
        // messages inside this account. Without it the domain looks perfect and receives nothing. Shown on
        // every domain's setup page because every account has to paste it into its own Mailgun.
        "inbound_webhook_url": inbound_webhook_url(),
        // The one thing the URL alone cannot do: prove the delivery came from Mailgun. The domain's own
        // webhook signing key does that, and the page states whether it has one rather than letting the
        // customer believe the URL was the whole job.
        "inbound_signing_key_configured": signing_key_configured,
        // The branded link the inbox tab opens, and whether it is the branded name or the shared one.
        "branded": branded.as_str(),
        "branded_url": format!("https://inbox.{}", domain),
        "mail_client_url": format!("https://{}", MAIL_CLIENT_HOST),
        // The create-mailbox button is enabled by this, so a customer can never make an address that
        // cannot receive. `provision_mailbox` refuses independently (a second door must not skip it).
        "can_create_mailbox": verified_live && receiving_ok && sending_ok,
        "mailgun_state": detail.get("state").cloned().unwrap_or(serde_json::Value::Null),
    })))
}

/// Mailgun's two record arrays, flattened into display rows.
fn flatten_records(
    section: &str,
    list: Option<&Vec<serde_json::Value>>,
    purpose: &str,
    out: &mut Vec<serde_json::Value>,
) {
    let Some(items) = list else { return };
    for r in items {
        let kind = r
            .get("record_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_uppercase();
        // v4 omits `name` on the MX rows (they apply at the domain root) and spells `valid` as the
        // STRING "valid"/"invalid". Both shapes are accepted here, or the two records a receiving
        // domain cannot live without were silently dropped and every other row read as invalid.
        let name = r
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or("@");
        let value = r.get("value").and_then(|v| v.as_str()).unwrap_or("");
        if value.is_empty() {
            continue;
        }
        let valid = match r.get("valid") {
            Some(serde_json::Value::Bool(b)) => *b,
            Some(serde_json::Value::String(s)) => s.eq_ignore_ascii_case("valid"),
            _ => r
                .get("is_active")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        };
        out.push(json!({
            "section": section,
            "type": kind,
            "name": name,
            "value": value,
            "priority": r.get("priority").cloned().unwrap_or(serde_json::Value::Null),
            "valid": valid,
            "purpose": purpose,
        }));
    }
}

/// Mailgun's full domain object (state, sending/receiving records). `None` when this credential
/// cannot read the domain — an undetermined answer, never "no records".
async fn mailgun_domain_detail(
    api_key: &str,
    domain: &str,
    region: &str,
) -> Option<serde_json::Value> {
    let base_url = if region == "eu" {
        "https://api.eu.mailgun.net"
    } else {
        "https://api.mailgun.net"
    };
    let client = reqwest::Client::new();

    // v4 FIRST: on this account `GET /v3/domains/<name>` answers the domain object WITHOUT
    // `receiving_dns_records` / `sending_dns_records` (measured 2026-11-11 on a verified domain), so
    // a v3-only read produced an EMPTY schedule page — the customer was asked to add records that
    // were never listed. v4 returns them beside the domain object; the two are merged into one
    // payload so callers that read `state` keep working.
    let v4 = client
        .get(format!("{}/v4/domains/{}", base_url, domain))
        .basic_auth("api", Some(api_key))
        .send()
        .await;
    if let Ok(resp) = v4 {
        if resp.status().is_success() {
            if let Ok(payload) = resp.json::<serde_json::Value>().await {
                let mut merged = payload
                    .get("domain")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({}));
                for key in ["receiving_dns_records", "sending_dns_records"] {
                    if let Some(v) = payload.get(key) {
                        merged[key] = v.clone();
                    }
                }
                if merged.get("state").is_some() || merged.get("receiving_dns_records").is_some() {
                    return Some(merged);
                }
            }
        }
    }

    // Older accounts: the v3 domain object carries the arrays inline.
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
        return None;
    }
    resp.json::<serde_json::Value>()
        .await
        .inspect_err(|e| tracing::warn!(error = %e, domain = %domain, "Mailgun domain payload did not parse"))
        .ok()?
        .get("domain")
        .cloned()
}

async fn add_mailgun_domain(
    state: &AppState,
    account_id: Uuid,
    req: &AddDomainRequest,
    label: &str,
    choices: &DomainChoices,
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

    // The inbound webhook signing key (kanban t_fbd607a7). Optional, and sealed here with the SAME
    // envelope as every other credential column in this app. A domain added without one is a
    // sending-only domain: the inbound receiver refuses every delivery for it (t_7f134e77), which
    // is exactly what this field exists to arm. An empty/absent value stores NULL — never a
    // ciphertext of nothing, so "no key" stays distinguishable from "a key is configured".
    // The signing key belongs to the MAILGUN receiver. A domain that receives through SendGrid has
    // no use for it — SendGrid's Inbound Parse signs nothing, so the key's only effect would be to
    // look armed while gating nothing. Refused rather than stored, and the panel never offers the
    // field on that branch (kanban t_04db9228).
    let wanted_webhook_key = req
        .webhook_signing_key
        .as_deref()
        .filter(|k| !k.trim().is_empty());
    if wanted_webhook_key.is_some() && choices.receiving != "mailgun" {
        return Err(AppError::BadRequest(format!(
            "A Mailgun webhook signing key only applies when this domain RECEIVES through Mailgun. \
             You chose {} — so the key would never be used. Choose Mailgun for receiving to paste \
             it, or leave it out and finish the {} setup instead.",
            choices.receiving, choices.receiving
        )));
    }
    let sealed_webhook: Option<String> = match wanted_webhook_key {
        Some(k) => Some(crate::secret_box::seal(account_id, k)?),
        None => None,
    };

    // `verified` follows what ACTUALLY receives. Mailgun's own `active` state describes the domain
    // in Mailgun's account: it is the receiving verdict only when Mailgun is the receiver. A domain
    // that sends through Mailgun but receives through SendGrid is verified by ITS MX pointing at
    // SendGrid (`POST /domains/:id/verify`), never by Mailgun's word about a different path.
    let verified = verified && choices.receiving == "mailgun";

    let row = sqlx::query_as::<_, PrivateEmailDomain>(
        r#"
        INSERT INTO private_email_domains (
            tenant_id, domain, mailgun_api_key, mailgun_region, label, api_key_id, provider_type,
            verified, webhook_signing_key_encrypted, inbound_provider, inbound_mode, inbound_token
        )
        VALUES ($1, $2, $3, $4, $5, $6, 'mailgun', $7, $8, $9, $10, $11)
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
    .bind(sealed_webhook)
    .bind(&choices.receiving)
    .bind(choices.inbound_mode())
    .bind(new_inbound_token())
    .fetch_one(&state.db)
    .await
    .map_err(AppError::Database)?;

    // Same shape as the list/update responses: the credential columns are `skip_serializing`, and
    // the webhook key is reported as presence + length (never the value), so the panel can say
    // whether inbound mail can be authenticated yet.
    let (keys, provider_keys) = load_credential_maps(&state.db, account_id).await?;
    let plan_sells_catch_all = feature_gate::catch_all_granted(&state.db, account_id).await?;
    Ok(Json(
        domain_json(
            &state,
            account_id,
            &row,
            &keys,
            &provider_keys,
            plan_sells_catch_all,
        )
        .await,
    ))
}

async fn add_smtp_domain(
    state: &AppState,
    account_id: Uuid,
    req: &AddDomainRequest,
    label: &str,
    choices: &DomainChoices,
) -> ApiResult<Json<serde_json::Value>> {
    // A signing key belongs to the MAILGUN RECEIVER, and receiving is now its own choice (kanban
    // t_04db9228): sending over SMTP while RECEIVING through Mailgun is a supported combination —
    // that is the customer who keeps Amazon SES for outbound and lets Mailgun deliver inbound — so
    // the key is required exactly when Mailgun is the receiver, and refused when it is not (where it
    // would be an inert credential that only looks armed).
    let wanted_webhook_key = req
        .webhook_signing_key
        .as_deref()
        .filter(|k| !k.trim().is_empty());
    if wanted_webhook_key.is_some() && choices.receiving != "mailgun" {
        tracing::warn!(
            tenant = %account_id,
            key_len = wanted_webhook_key.map(str::len).unwrap_or(0),
            receiving = %choices.receiving,
            "rejected a webhook signing key — this domain does not receive through Mailgun"
        );
        return Err(AppError::BadRequest(format!(
            "An inbound webhook signing key applies to Mailgun's receiver only. You chose {} for \
             receiving, so the key would never be used.",
            choices.receiving
        )));
    }
    // The "receiving through Mailgun needs its signing key" rule is enforced ONCE, in `add_domain`,
    // before either door is chosen — so the two doors cannot drift apart again.
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
    //
    // The exception is a domain whose RECEIVER is Mailgun (kanban t_04db9228): the records page and
    // the re-check read Mailgun's own API for this domain, so the customer's Mailgun key is sealed
    // into this same column. Sending still goes over SMTP — `provider_type` is what the send path
    // reads — so the two choices stay independent.
    let sealed_webhook: Option<String> = match wanted_webhook_key {
        Some(k) => Some(crate::secret_box::seal(account_id, k)?),
        None => None,
    };
    let mailgun_key_for_receiving = if choices.receiving == "mailgun" {
        let raw = req
            .mailgun_api_key
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .ok_or_else(|| {
                AppError::BadRequest(
                    "Receiving through Mailgun needs the Mailgun API key for this domain (Mailgun \
                     → Settings → API keys) so its records can be shown and checked. Your sending \
                     stays on SMTP; nothing else changes."
                        .into(),
                )
            })?;
        crate::secret_box::seal(account_id, raw)?
    } else {
        String::new()
    };

    // The REGION is not decorative. A domain that RECEIVES through Mailgun is checked against
    // Mailgun's own API (the records page and every re-check), and an EU account answered from
    // api.mailgun.net reads as a credential failure on every call. This used to be the literal 'us',
    // so an EU customer who chose Mailgun as the RECEIVER — the combination the receiving select
    // exists for — silently got a domain whose records could never be read. Anything that is not
    // "eu" is stored as "us" (the request's own default), so an unrecognised value can never become
    // a URL segment.
    let mailgun_region = if req.mailgun_region.trim().eq_ignore_ascii_case("eu") {
        "eu"
    } else {
        "us"
    };

    let row = sqlx::query_as::<_, PrivateEmailDomain>(
        r#"
        INSERT INTO private_email_domains (
            tenant_id, domain, mailgun_api_key, mailgun_region, label,
            provider_type, smtp_host, smtp_port, smtp_username, smtp_password_encrypted, smtp_tls,
            inbound_mode, inbound_provider, webhook_signing_key_encrypted, inbound_token
        )
        VALUES ($1, $2, $3, $14, $4, 'smtp', $5, $6, $7, $8, $9, $10, $11, $12, $13)
        RETURNING *
        "#,
    )
    .bind(account_id)
    .bind(&req.domain)
    .bind(&mailgun_key_for_receiving)
    .bind(label)
    .bind(smtp_host)
    .bind(req.smtp_port.unwrap_or(587))
    .bind(smtp_username)
    .bind(&encrypted_smtp_password)
    .bind(req.smtp_tls)
    .bind(choices.inbound_mode())
    .bind(&choices.receiving)
    .bind(sealed_webhook)
    .bind(new_inbound_token())
    .bind(mailgun_region)
    .fetch_one(&state.db)
    .await
    .map_err(AppError::Database)?;

    let (keys, provider_keys) = load_credential_maps(&state.db, account_id).await?;
    let plan_sells_catch_all = feature_gate::catch_all_granted(&state.db, account_id).await?;
    Ok(Json(
        domain_json(
            &state,
            account_id,
            &row,
            &keys,
            &provider_keys,
            plan_sells_catch_all,
        )
        .await,
    ))
}
