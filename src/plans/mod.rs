//! Plans module — one live endpoint (see `handlers`).
//!
//! `router()` and `models` were deleted with the unreachable plan CRUD they served: the router was
//! never nested in `main.rs`, and `models::Plan` was the last place declaring the phantom `plans`
//! columns (`max_deals`, `max_users`, `max_storage_mb`, `payment_link` — none exist in the
//! database). The live paths are `/api/billing/plans*` and `/api/admin/*`.

pub mod handlers;
