# Native Apps Module — Verification Report

**Date:** 2026-06-14  
**Project:** CRM Swift  
**Module:** `native_apps` (connectors + handlers + models + migration 025)

---

## 1. Compilation
**Result: ✅ PASS** (after fixes applied)

`cargo check` now passes with **zero errors, zero warnings**.

### Issues Found & Fixed

| Issue | File(s) | Fix |
|-------|---------|-----|
| `AppError::Forbidden("msg".into())` — `Forbidden` is a unit variant, not a function | `handlers.rs` (4 occurrences) | Replaced with `AppError::Forbidden` |
| `sqlx::query_as::<_, serde_json::Value>` — `serde_json::Value` does not implement `FromRow` | `handlers.rs` (5 occurrences) | Replaced with `sqlx::query` + `Row::get` manual mapping or `query_scalar` for single JSONB columns |
| Unused import `HashMap` | `handlers.rs` | Removed |

---

## 2. `mod` Declarations vs Actual Files
**Result: ✅ PASS**

| Declaration in `mod.rs` | Actual File |
|-------------------------|-------------|
| `pub mod models;` | `src/native_apps/models.rs` ✅ |
| `pub mod handlers;` | `src/native_apps/handlers.rs` ✅ |
| `pub mod connectors;` | `src/native_apps/connectors.rs` ✅ |

| Declaration in `connectors.rs` | Actual File |
|-------------------------------|-------------|
| `pub mod adaswift;` | `src/native_apps/connectors/adaswift.rs` ✅ |
| `pub mod funnelswift;` | `src/native_apps/connectors/funnelswift.rs` ✅ |
| `pub mod cheatlayer;` | `src/native_apps/connectors/cheatlayer.rs` ✅ |
| `pub mod workflowswift;` | `src/native_apps/connectors/workflowswift.rs` ✅ |
| `pub mod missedcall_responder;` | `src/native_apps/connectors/missedcall_responder.rs` ✅ |
| `pub mod multi_directory;` | `src/native_apps/connectors/multi_directory.rs` ✅ |

All 9 declarations match their actual files.

---

## 3. Main.rs Integration
**Result: ✅ PASS**

- `main.rs` contains: `pub mod native_apps;` ✅
- `main.rs` contains: `.nest("/api/native", native_apps::router(state.clone()))` ✅

All other module registrations (`auth`, `tenants`, `contacts`, etc.) remain intact.

---

## 4. Migration 025 SQL Validation
**Result: ✅ PASS** (with minor note)

The migration `025_create_native_app_connections.sql` creates 4 tables + 1 seed with proper syntax:

| Table | Purpose | Status |
|-------|---------|--------|
| `native_apps` | Global app registry (seed data) | ✅ |
| `app_connections` | Per-tenant credential storage | ✅ |
| `app_sync_logs` | Sync history log | ✅ |
| `app_admin_configs` | Admin-level global configs | ✅ |
| `ada_campaign_triggers` | RETIRED — dropped by migration 109 (kanban t_434b240b): nothing read it | ❌ |

**Checks performed:**
- All `CREATE TABLE IF NOT EXISTS` syntax is valid ✅
- All column names, types, constraints are consistent ✅
- All `REFERENCES` point to existing tables (`tenants(id)`) ✅
- All index names are unique within the migration ✅
- `INSERT ... ON CONFLICT (slug) DO NOTHING` is correct ✅
- JSONB casts (`'{}'::jsonb`) are valid ✅
- Seed data has 6 apps matching the 6 connectors ✅

**⚠️ Note:** The `ada_campaign_triggers.trigger_on` CHECK constraint allowed 8 values (the four core
ones plus `referral_confirmed`, `commission_earned`, `payout_processed`, `affiliate_activated`), and
this note used to claim the Rust handler only accepted 4 — it actually accepted all 8, so an operator
could map a trigger to an event no emitter could ever produce. RETIRED 2026-10-02 (kanban t_434b240b):
the table, the handler and its CHECK constraint are gone (migration 109) because nothing ever read
`trigger_on` to fire a campaign.

---

## 5. Connector Function Signature Consistency
**Result: ✅ PASS**

All 6 connector files use identical function signatures:

| Function | Signature | Status |
|----------|-----------|--------|
| `test` | `pub async fn test(creds: &serde_json::Value) -> (bool, String)` | ✅ All 6 |
| `push_entity` | `pub async fn push_entity(creds: &serde_json::Value, entity_type: &str, data: &serde_json::Value) -> Result<serde_json::Value, String>` | ✅ All 6 |
| `pull_entity` | `pub async fn pull_entity(creds: &serde_json::Value, entity_type: &str, filters: &HashMap<String, String>) -> Result<serde_json::Value, String>` | ✅ All 6 |
| `get_meta` | `pub fn get_meta() -> serde_json::Value` | ✅ All 6 |

Connectors verified (vocabulary re-measured against each LIVE target 2026-10-02, kanban
t_b43e4604 — every declared name below is a route a live request reached; see the module
header of each file for the measurement table):
1. ✅ **adaswift** — Admin, AdaSwift admin bearer token + base_url, entities: push(client), pull(reports). The five names originally declared here (push contact/client/trigger_campaign, pull campaigns/reports) all targeted `/api/*` paths AdaSwift does not serve (404 live); re-pointed at `POST /api/v1/clients` + `GET /api/v1/scan-reports`, and `contact`/`trigger_campaign`/`campaigns` retired because AdaSwift has no such entity (kanban t_8b81b1dd).
2. ⚠️ **cheatlayer** — Admin, API key + base_url, entities: push(workflow/job/template), pull(workflows/jobs/templates/logs). NOT MEASURED: CheatLayer is a third-party hosted RPA tool with no local deployment, so no live target exists here; its `test()` (`/api/health`) and every entity path may or may not resolve — carded as a follow-up (needs a CheatLayer account). Note its `pull_entity` builds `{base}/{entity}` while `push_entity` builds `{base}/api/{entity}` and `test()` uses an `X-API-Key` header — internally inconsistent, unverified either way.
3. ✅ **funnelswift** — Admin+Tenant, API key + base_url, entities: push(lead/funnel/tag), pull(leads/funnels/tags). `contact`/`contacts` retired (no such route or table). The prefix was also wrong: it built `{base}/v1/…` and probed `{base}/v1/health`, which nginx's SPA fallback answers 200 text/html to for ANY credential (a vacuous test); re-pointed at `{base}/api/v1/…` (kanban t_e8a7f651 / t_7225a94d / t_b43e4604).
4. ✅ **workflowswift** — Admin+Tenant, API key + base_url, entities: push(workflow/trigger), pull(workflows/instances/credits). Every old path (`/health`, `/api/workflow`, `/api/trigger`, `/api/workflows`, `/api/runs`, `/api/credits`) answered 404; re-pointed at `/api/v1/workflows`, `/api/v1/workflows/trigger`, `/api/v1/instances` and `/api/v1/credits/balance`, and `runs` is declared as `instances` — WorkflowSwift's own name for a run.
5. ✅ **missedcall_responder** — Admin+Tenant, API key (bearer token) + base_url, entities: push(lead/contact), pull(leads/contacts/messages/call_logs). Every old path answered 404 (bare `/api/*` never reaches its router); `tenant_config`/`sms_reply` (push) and `tenant_settings` (pull) retired — no such entity on the far side; `conversations` is declared as `messages`, the far side's name.
6. ✅ **multi_directory** — Admin, API key (bearer token) + base_url, entities: push(none), pull(listings/reviews/analytics). Every old path answered 404; the push list is EMPTY because the only business create route (`POST /api/v1/directories/{slug}/businesses`) needs a directory slug the collected credentials do not carry, and `businesses`/`followup_status` (pull) have no route at all — the far side's business collection is `/api/v1/listings`.

---

## 6. Source File & Line Count

| Scope | Files | Lines |
|-------|-------|-------|
| **Entire project** (`src/`) | **90 .rs files** | **8,282 lines** |
| **native_apps module only** | **10 .rs files** | **1,390 lines** |

Breakdown of native_apps files:

| File | Lines |
|------|-------|
| `mod.rs` | 43 |
| `handlers.rs` | 453 |
| `models.rs` | 97 |
| `connectors.rs` | 137 |
| `connectors/adaswift.rs` | 138 |
| `connectors/cheatlayer.rs` | 99 |
| `connectors/funnelswift.rs` | 92 |
| `connectors/workflowswift.rs` | 102 |
| `connectors/missedcall_responder.rs` | 119 |
| `connectors/multi_directory.rs` | 110 |
| **Total** | **1,390** |

---

## Summary

| Check | Result |
|-------|--------|
| `cargo check` (compilation) | ✅ Passed (0 errors, 0 warnings) |
| `mod` declarations match files | ✅ All 9 match |
| `main.rs` has `pub mod native_apps` | ✅ Yes |
| `main.rs` has `.nest("/api/native", ...)` | ✅ Yes |
| Migration 025 SQL syntax | ✅ Valid (1 minor inconsistency noted) |
| Connector function signatures | ✅ All 6 consistent |
| Total source files | 90 |
| Total lines of code | 8,282 |

**Overall: ✅ PASS — Native apps module is ready.**
