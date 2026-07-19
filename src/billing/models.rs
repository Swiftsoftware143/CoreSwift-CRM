use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use rust_decimal::Decimal;

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Plan {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub price_monthly: Decimal,
    pub price_yearly: Decimal,
    pub features: serde_json::Value,
    pub checkout_url: Option<String>,
    pub payment_provider: Option<String>,
    pub thank_you_url: Option<String>,
    pub is_active: bool,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePlanRequest {
    pub name: String,
    pub slug: String,
    pub description: Option<String>,
    pub price_monthly: f64,
    pub price_yearly: f64,
    pub features: serde_json::Value,
    pub checkout_url: Option<String>,
    pub payment_provider: Option<String>,
    pub thank_you_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdatePlanRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub price_monthly: Option<f64>,
    pub price_yearly: Option<f64>,
    pub features: Option<serde_json::Value>,
    pub checkout_url: Option<String>,
    pub payment_provider: Option<String>,
    pub thank_you_url: Option<String>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct TenantPlan {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub plan_id: Uuid,
    pub status: String,
    pub billing_cycle: String,
    pub trial_ends_at: Option<DateTime<Utc>>,
    pub current_period_starts_at: DateTime<Utc>,
    pub current_period_ends_at: DateTime<Utc>,
    pub feature_overrides: serde_json::Value,
    pub canceled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSubscriptionRequest {
    pub plan_id: Uuid,
    pub billing_cycle: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSubscriptionRequest {
    pub plan_id: Option<Uuid>,
    pub billing_cycle: Option<String>,
    pub feature_overrides: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct FeaturesResponse {
    pub plan: PlanSummary,
    pub features: serde_json::Value,
    pub limits: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct PlanSummary {
    pub id: Uuid,
    pub name: String,
    pub slug: String,
    pub checkout_url: Option<String>,
    pub payment_provider: Option<String>,
    pub thank_you_url: Option<String>,
}

/// Merge overrides into plan features, override wins
pub fn merge_features(plan_features: &serde_json::Value, overrides: &serde_json::Value) -> (serde_json::Value, serde_json::Value) {
    use serde_json::{Map, Value};
    let mut features = Map::new();
    let mut limits = Map::new();

    if let Some(pf) = plan_features.as_object() {
        for (k, v) in pf {
            let val = overrides.get(k).unwrap_or(v);
            if v.is_number() {
                limits.insert(k.clone(), val.clone());
            } else {
                features.insert(k.clone(), val.clone());
            }
        }
    }
    // Add any override-only keys
    if let Some(ov) = overrides.as_object() {
        for (k, v) in ov {
            if !plan_features.as_object().is_some_and(|pf| pf.contains_key(k)) {
                if v.is_number() {
                    limits.insert(k.clone(), v.clone());
                } else {
                    features.insert(k.clone(), v.clone());
                }
            }
        }
    }

    (Value::Object(features), Value::Object(limits))
}
