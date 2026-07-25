use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use rust_decimal::Decimal;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Affiliate {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub user_id: Uuid,
    pub code: String,
    pub commission_rate: Decimal,
    pub commission_type: String,
    pub total_earned: Decimal,
    pub total_paid: Decimal,
    pub referral_count: i32,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Referral {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub referred_tenant_id: Option<Uuid>,
    pub referred_email: Option<String>,
    pub status: String,
    pub commission_amount: Decimal,
    pub paid_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct CommissionPayout {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub amount: Decimal,
    pub status: String,
    pub payment_method: Option<String>,
    pub paid_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateAffiliateRequest {
    pub commission_rate: Option<f64>,
    pub commission_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAffiliateRequest {
    pub commission_rate: Option<f64>,
    pub commission_type: Option<String>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct AffiliateStats {
    pub total_referrals: i64,
    pub pending_referrals: i64,
    pub converted_referrals: i64,
    pub total_earned: f64,
    pub total_paid: f64,
    pub pending_payout: f64,
}

// ── Affiliate Products (the product board) ──

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AffiliateProduct {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub price: Decimal,
    pub commission_rate: Decimal,
    pub commission_type: String,
    pub commission_amount: Decimal,
    pub tag_id: Option<Uuid>,
    pub image_url: Option<String>,
    pub checkout_url: Option<String>,
    pub is_active: bool,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateProductRequest {
    pub name: String,
    pub description: Option<String>,
    pub price: f64,
    pub commission_rate: Option<f64>,
    pub commission_type: Option<String>,
    pub commission_amount: Option<f64>,
    pub tag_id: Option<Uuid>,
    pub image_url: Option<String>,
    pub checkout_url: Option<String>,
    pub sort_order: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProductRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub price: Option<f64>,
    pub commission_rate: Option<f64>,
    pub commission_type: Option<String>,
    pub commission_amount: Option<f64>,
    pub tag_id: Option<Uuid>,
    pub image_url: Option<String>,
    pub checkout_url: Option<String>,
    pub is_active: Option<bool>,
    pub sort_order: Option<i32>,
}

/// Affiliate self-serve: pick products to promote from FunnelSwift back-end
#[derive(Debug, Deserialize)]
pub struct SelectProductRequest {
    pub product_id: Uuid,
    pub promo_link: Option<String>,
    pub custom_commission_rate: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AffiliateProductSelection {
    pub id: Uuid,
    pub affiliate_id: Uuid,
    pub product_id: Uuid,
    pub is_active: bool,
    pub promo_link: Option<String>,
    pub custom_commission_rate: Option<serde_json::Value>,
    pub selected_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Generate a unique affiliate code from a name
pub fn generate_code(name: &str) -> String {
    use rand::Rng;
    let base = name.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .take(10)
        .collect::<String>();
    let suffix: u32 = rand::thread_rng().gen_range(100..999);
    format!("{}{}", base, suffix)
}
