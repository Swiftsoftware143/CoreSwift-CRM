//! Native App Connector — data models

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// ── Registered app definitions (seed data, managed by admin) ──

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct NativeApp {
    pub id: Uuid,
    pub slug: String, // e.g. "adaswift", "funnelswift", "cheatlayer"
    pub name: String, // e.g. "AdaSwift Console"
    pub description: String,
    pub auth_type: String,              // "api_key" | "oauth2" | "basic"
    pub auth_config: serde_json::Value, // { "fields": ["api_key","base_url"], "oauth_scopes": [...] }
    pub access_level: String,           // "admin" | "admin_tenant"
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ── Tenant-specific app connections ──

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AppConnection {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub app_id: Uuid,
    pub credentials: serde_json::Value, // encrypted per-tenant API keys / tokens
    pub config: serde_json::Value,      // per-tenant settings (e.g. which lists to sync)
    pub status: String,                 // "connected" | "disconnected" | "error"
    pub last_test_at: Option<DateTime<Utc>>,
    pub last_test_ok: Option<bool>,
    pub error_message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ── Sync history ──

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AppSyncLog {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub app_connection_id: Uuid,
    pub direction: String,   // "push" | "pull"
    pub entity_type: String, // "contact" | "list" | "tag"
    pub records_processed: i32,
    pub records_succeeded: i32,
    pub records_failed: i32,
    pub error_log: Option<serde_json::Value>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub status: String, // "running" | "completed" | "failed"
}

// ── API request/response types ──

#[derive(Debug, Deserialize)]
pub struct ConnectAppRequest {
    pub app_slug: String,
    pub credentials: serde_json::Value,
    pub config: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct PullRequest {
    pub entity_type: String, // "contacts" | "lists" | "tags"
    pub filters: Option<HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
pub struct PushRequest {
    pub entity_type: String,
    pub data: serde_json::Value,
}

// ── Connection test result ──

#[derive(Debug, Serialize, Deserialize)]
pub struct ConnectionTestResult {
    pub success: bool,
    pub message: String,
    pub latency_ms: Option<i64>,
}
