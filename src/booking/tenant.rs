//! Handing the signed-in account to the booking engine.
//!
//! The engine was written against a `TenantId` extension it expects the host's auth middleware to
//! insert. CoreSwift's global auth middleware inserts `Claims` (whose `aid` is the account id) and
//! knows nothing about the engine, so this one-line adapter bridges them rather than teaching either
//! side about the other.
//!
//! It runs INSIDE the global auth layer, so an unauthenticated caller has already been refused by
//! the time this executes: a request that reaches here without `Claims` gets no `TenantId`, and every
//! engine handler refuses it (`TenantId` is a required extractor). There is deliberately no
//! default tenant — a missing account must never read as "the first tenant".

use axum::{extract::Request, middleware::Next, response::Response};
use uuid::Uuid;

use super::models::TenantId;
use crate::auth::models::Claims;

pub async fn accept_tenant_from_claims(mut req: Request, next: Next) -> Response {
    if let Some(claims) = req.extensions().get::<Claims>() {
        if let Ok(account_id) = Uuid::parse_str(&claims.aid) {
            req.extensions_mut().insert(TenantId(account_id));
        }
    }
    next.run(req).await
}
