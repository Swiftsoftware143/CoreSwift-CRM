//! "How to pay" — the operator's own payment details, for an install with no card provider.
//!
//! CoreSwift has no till: `checkout_sessions` and its provider webhooks were dropped by migration
//! 097, and every paid plan is bought by ASKING (the customer presses "Request this plan", a
//! `tickets` row with `source = 'plan_request'` is filed, and a person switches the workspace on).
//! What was missing is the last step — a buyer who is ready to send money had nowhere to send it,
//! because nothing in the product said how this business takes payment (a bank transfer, an
//! invoice, another rail).
//!
//! The operator writes that once, in their own words, on the console's **How Customers Pay**
//! screen (`GET|PUT /api/admin/billing-settings`). The same words then reach a buyer on:
//!
//!   * the PUBLIC price list (`GET /api/v1/public/plans`), which the marketing pricing page reads
//!     — so a visitor sees them before signing up;
//!   * the customer console's plan ladder, which reads the same public payload.
//!
//! Absent or empty stays exactly as before: nothing is shown anywhere. That is the feature switch,
//! and it is the operator's — this module never invents payment details.
//!
//! Stored in `admin_settings` key `billing` as `{"payment_instructions": "..."}`. The writer MERGES
//! this one field into the row (jsonb `||`) so a later fleet field stored under the same key is
//! never dropped by a save from this screen.
//!
//! Fleet parity: FunnelSwift, IncentiveSwift, ADASwift, WorkflowSwift and MissedCall Respondr each
//! shipped the same module on 2026-11-16..18; this is the CoreSwift side.

use sqlx::PgPool;

/// The longest blob the console may store. Generous enough for bank details plus a paragraph of
/// guidance, small enough that it cannot be used as a message board.
pub const MAX_LEN: usize = 4000;

/// The operator's saved words, or `None` when the feature is off (absent row, absent field, or an
/// empty/whitespace string). Callers treat `None` as "say nothing new".
pub async fn load(pool: &PgPool) -> Option<String> {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT value->>'payment_instructions' FROM admin_settings WHERE key = 'billing'",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();

    raw.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Save the operator's words. An empty string is a deliberate CLEAR (the feature goes back to off),
/// never a silent no-op.
pub async fn save(pool: &PgPool, text: &str) -> Result<(), String> {
    let text = text.trim();
    if text.chars().count() > MAX_LEN {
        return Err(format!(
            "The payment instructions are too long ({} characters). Keep them under {}.",
            text.chars().count(),
            MAX_LEN
        ));
    }
    sqlx::query(
        "INSERT INTO admin_settings (key, value, description, updated_at)
         VALUES ('billing',
                 jsonb_build_object('payment_instructions', $1::text),
                 'Platform billing settings (admin-editable): how a customer pays while card payments are off',
                 NOW())
         ON CONFLICT (key) DO UPDATE
            SET value = admin_settings.value || jsonb_build_object('payment_instructions', $1::text),
                updated_at = NOW()",
    )
    .bind(text)
    .execute(pool)
    .await
    .map_err(|e| format!("Failed to save the payment instructions: {e}"))?;
    Ok(())
}
